use chrono::{DateTime, Utc};
use chrono_tz::Asia::Kolkata;
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

pub async fn dedupe_or_settle(
    pool: &PgPool,
    user_id: Uuid,
    span_id: Uuid,
    data: &Value,
) -> Result<(), sqlx::Error> {
    let direction = data.get("direction").and_then(Value::as_str);
    let is_due = direction == Some("due");
    let reference = data.get("reference").and_then(Value::as_str);
    let account_hint = data.get("account_hint").and_then(Value::as_str);
    let merchant = data.get("merchant").and_then(Value::as_str);
    let amount = data.get("amount").and_then(Value::as_f64);
    let effective: DateTime<Utc> = data
        .get("occurred_at")
        .and_then(Value::as_str)
        .and_then(|v| DateTime::parse_from_rfc3339(v).ok())
        .map(|v| v.with_timezone(&Utc))
        .unwrap_or_else(Utc::now);

    let fp = fingerprint(is_due, reference, account_hint, merchant, amount, effective);
    let lock_key = i64::from_le_bytes(Sha256::digest(&fp)[0..8].try_into().unwrap());

    let mut tx = pool.begin().await?;

    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(lock_key)
        .execute(&mut *tx)
        .await?;

    let existing = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM spans WHERE user_id = $1 AND source = 'sms' \
         AND data->>'fingerprint' = $2 AND id <> $3 ORDER BY created_at LIMIT 1",
    )
    .bind(user_id)
    .bind(&fp)
    .bind(span_id)
    .fetch_optional(&mut *tx)
    .await?;

    if let Some(existing_id) = existing {
        sqlx::query(
            "UPDATE spans SET data = data || jsonb_build_object( \
                'duplicate_count', COALESCE((data->>'duplicate_count')::int, 0) + 1), \
                version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(existing_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query("DELETE FROM spans WHERE id = $1")
            .bind(span_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Ok(());
    }

    sqlx::query(
        "UPDATE spans SET data = data || jsonb_build_object('fingerprint', $2::text) WHERE id = $1",
    )
    .bind(span_id)
    .bind(&fp)
    .execute(&mut *tx)
    .await?;

    if direction == Some("debit")
        && let (Some(account), Some(amount)) = (account_hint, amount)
    {
        sqlx::query(
            "UPDATE spans SET status = 'done', completed_at = now(), \
                data = data || jsonb_build_object('settled_by_span_id', ($2::uuid)::text), \
                version = version + 1, updated_at = now() \
             WHERE user_id = $1 AND source = 'sms' AND status = 'planned' \
               AND data->>'direction' = 'due' AND data->>'account_hint' = $3 \
               AND round((data->>'amount')::numeric, 2) = round($4::numeric, 2) \
               AND id <> $2::uuid",
        )
        .bind(user_id)
        .bind(span_id)
        .bind(account)
        .bind(amount)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

fn fingerprint(
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

fn normalize_merchant(merchant: &str) -> String {
    merchant
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_lowercase()
}
