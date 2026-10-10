use chrono::{DateTime, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn dedupe_or_settle(
    pool: &PgPool,
    user_id: Uuid,
    timeline_event_id: Uuid,
    data: &Value,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    dedupe_or_settle_in_transaction(&mut tx, user_id, timeline_event_id, data).await?;
    tx.commit().await
}

pub async fn dedupe_or_settle_in_transaction(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
    timeline_event_id: Uuid,
    data: &Value,
) -> Result<(), sqlx::Error> {
    let direction = data.get("direction").and_then(Value::as_str);
    let is_due = direction == Some("due");
    let is_statement = direction == Some("statement")
        || data.get("is_statement").and_then(Value::as_bool) == Some(true);
    let is_repayment = direction == Some("repayment");
    let is_refund = direction == Some("refund");
    let is_transfer = direction == Some("transfer");
    let reference = data.get("reference").and_then(Value::as_str);
    let account_hint = data.get("account_hint").and_then(Value::as_str);
    let merchant = data.get("merchant").and_then(Value::as_str);
    let amount = data.get("amount").and_then(Value::as_f64);
    let effective: DateTime<Utc> =
        sqlx::query_scalar("SELECT occurred_at FROM timeline_events WHERE id=$1 AND user_id=$2")
            .bind(timeline_event_id)
            .bind(user_id)
            .fetch_one(&mut **tx)
            .await?;
    let currency = data
        .get("currency")
        .and_then(Value::as_str)
        .unwrap_or("unknown");

    let is_spending = data.get("is_spending").and_then(Value::as_bool) == Some(true)
        && direction == Some("debit")
        && !is_due
        && !is_statement
        && !is_repayment
        && !is_transfer
        && !is_refund;

    let base_fp = fingerprint(is_due, reference, account_hint, merchant, amount, effective);
    let fp = if reference.is_some_and(|r| r.trim().len() >= 4) && amount.is_some() {
        hex::encode(Sha256::digest(format!(
            "{user_id}|{}|{currency}|{base_fp}",
            direction.unwrap_or("unknown")
        )))
    } else {
        format!("financial:{timeline_event_id}")
    };
    let lock_key = i64::from_le_bytes(Sha256::digest(&fp)[0..8].try_into().unwrap());

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut **tx)
        .await?;

    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM timeline_events WHERE user_id = $1 \
         AND content->>'fingerprint' = $2 AND record_state='active' AND id <> $3 ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .bind(&fp)
    .bind(timeline_event_id)
    .fetch_optional(&mut **tx)
    .await?;

    if let Some(existing_id) = existing {
        sqlx::query(
            "UPDATE timeline_events SET content = content || jsonb_build_object( \
                'duplicate_count', COALESCE((content->>'duplicate_count')::int, 0) + 1), \
                revision = revision + 1, updated_at = now() WHERE id = $1",
        )
        .bind(existing_id)
        .execute(&mut **tx)
        .await?;
        sqlx::query("DELETE FROM timeline_evidence duplicate USING timeline_evidence original WHERE duplicate.timeline_event_id=$2 AND original.timeline_event_id=$1 AND duplicate.user_id=$3 AND original.user_id=$3 AND duplicate.evidence_hash=original.evidence_hash")
            .bind(existing_id).bind(timeline_event_id).bind(user_id).execute(&mut **tx).await?;
        sqlx::query("UPDATE timeline_evidence SET timeline_event_id=$1 WHERE timeline_event_id=$2 AND user_id=$3")
            .bind(existing_id).bind(timeline_event_id).bind(user_id).execute(&mut **tx).await?;
        sqlx::query("UPDATE timeline_events SET record_state='superseded',content=content || jsonb_build_object('superseded_by',$2::text),revision=revision+1,updated_at=now() WHERE id=$1 AND user_id=$3")
            .bind(timeline_event_id).bind(existing_id).bind(user_id)
            .execute(&mut **tx)
            .await?;
        return Ok(());
    }

    let net_amount = if is_refund {
        amount.map(|a| -a.abs())
    } else if is_spending {
        amount.map(|a| a.abs())
    } else {
        Some(0.0)
    };

    sqlx::query(
        "UPDATE timeline_events SET revision=revision+1, updated_at=now(), \
                content = content || jsonb_build_object( \
                    'fingerprint', $2::text, \
                    'is_spending', $3::boolean, \
                    'is_refund', $4::boolean, \
                    'is_statement', $5::boolean, \
                    'is_repayment', $6::boolean, \
                    'is_due', $7::boolean, \
                    'is_transfer', $8::boolean, \
                    'net_amount', $9::numeric \
                ) \
         WHERE id = $1",
    )
    .bind(timeline_event_id)
    .bind(&fp)
    .bind(is_spending)
    .bind(is_refund)
    .bind(is_statement)
    .bind(is_repayment)
    .bind(is_due)
    .bind(is_transfer)
    .bind(net_amount)
    .execute(&mut **tx)
    .await?;

    if (direction == Some("debit") || is_repayment)
        && reference.is_some_and(|r| r.trim().len() >= 4)
    {
        let ref_str = reference.unwrap().trim();
        sqlx::query(
            "UPDATE timeline_events SET \
                content = content || jsonb_build_object('settled_by_event_id', ($2::uuid)::text, 'settled_at', now()), \
                revision = revision + 1, updated_at = now() \
             WHERE user_id = $1 \
               AND (content->>'direction' = 'due' OR content->>'document_kind' = 'bill') \
               AND ( \
                   content->>'reference' = $3 \
                   OR content->>'invoice_number' = $3 \
                   OR content->>'bill_id' = $3 \
               ) \
               AND COALESCE(content->>'currency','unknown')=$4 \
               AND id <> $2::uuid",
        )
        .bind(user_id)
        .bind(timeline_event_id)
        .bind(ref_str)
        .bind(currency)
        .execute(&mut **tx)
        .await?;
    }

    Ok(())
}

pub fn fingerprint(
    is_due: bool,
    reference: Option<&str>,
    account_hint: Option<&str>,
    merchant: Option<&str>,
    amount: Option<f64>,
    effective: DateTime<Utc>,
) -> String {
    let who = match reference {
        Some(reference) if reference.len() >= 4 => format!("ref:{reference}"),
        _ => format!(
            "acct:{}|merchant:{}",
            account_hint.unwrap_or(""),
            merchant.map(normalize_merchant).unwrap_or_default()
        ),
    };
    let day = effective.with_timezone(&Kolkata).date_naive();
    let amount = amount.map(|a| format!("{a:.2}")).unwrap_or_default();
    let kind = if is_due { "due" } else { "money" };
    hex::encode(Sha256::digest(format!("{kind}|{who}|{amount}|{day}")))
}

pub fn normalize_merchant(merchant: &str) -> String {
    merchant
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}
