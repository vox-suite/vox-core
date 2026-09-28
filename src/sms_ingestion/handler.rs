use chrono::{DateTime, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Asia::Kolkata;
use futures_util::{StreamExt, stream};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
use vox_shared::sms::{SMS_SEED_CATEGORIES, normalize_category};

use crate::{
    agents::sms_extractor::{ExtractedSmsEvent, SmsExtracting, SmsPrompt},
    core_api_client::{DeviceDispatcher, DispatchDeviceRequest},
    db::Db,
    domain::spans::{NewSpan, SpanStatus},
    sms_ingestion::SmsMessage,
    storage::spans::SpanRepository,
};

const CLASSIFY_CONCURRENCY: usize = 6;
const MAX_KNOWN_CATEGORIES: i64 = 40;
const MAX_ATTRIBUTES: usize = 20;
const MAX_ATTRIBUTE_TEXT: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum SmsBatchHandlerError {
    #[error("sms batch storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("sms batch not found")]
    NotFound,
}

#[derive(Clone)]
pub struct SmsBatchHandler {
    db: Db,
    extractor: Arc<dyn SmsExtracting>,
    device_dispatcher: Option<Arc<dyn DeviceDispatcher>>,
}

#[derive(Default)]
struct Tally {
    otp: usize,
    duplicate: usize,
    classify_failed: usize,
    not_relevant: usize,
    written: usize,
    merged: usize,
}

enum Stored {
    Written(Uuid),
    Merged(Uuid),
}

impl SmsBatchHandler {
    pub fn new(
        db: Db,
        extractor: Arc<dyn SmsExtracting>,
        device_dispatcher: Option<Arc<dyn DeviceDispatcher>>,
    ) -> Self {
        Self {
            db,
            extractor,
            device_dispatcher,
        }
    }

    pub async fn handle(&self, batch_id: Uuid) -> Result<(), SmsBatchHandlerError> {
        let row = sqlx::query("SELECT user_id, messages, status FROM sms_batches WHERE id = $1")
            .bind(batch_id)
            .fetch_optional(self.db.pool())
            .await?
            .ok_or(SmsBatchHandlerError::NotFound)?;

        let status: String = row.get("status");
        if status != "pending" {
            return Ok(());
        }

        let user_id: Uuid = row.get("user_id");
        let messages_json: serde_json::Value = row.get("messages");
        let messages: Vec<SmsMessage> = serde_json::from_value(messages_json).unwrap_or_default();
        let total = messages.len();
        let mut tally = Tally::default();

        let mut seen_in_batch = HashSet::new();
        let mut todo: Vec<(SmsMessage, String)> = Vec::new();
        for message in messages {
            let digest = message_digest(&message);
            if !seen_in_batch.insert(digest.clone()) {
                tally.duplicate += 1;
                continue;
            }
            if looks_like_otp(&message.body) {
                tally.otp += 1;
                self.mark_processed(user_id, &digest, "otp", None).await?;
                continue;
            }
            if self.already_processed(user_id, &digest).await? {
                tally.duplicate += 1;
                continue;
            }
            todo.push((message, digest));
        }

        let known_categories = self.known_categories(user_id).await?;
        let classified: Vec<_> = stream::iter(todo)
            .map(|(message, digest)| {
                let known = known_categories.clone();
                async move {
                    let event = self.classify(user_id, &message, &known).await;
                    (message, digest, event)
                }
            })
            .buffered(CLASSIFY_CONCURRENCY)
            .collect()
            .await;

        for (message, digest, event) in classified {
            let Some(event) = event else {
                tally.classify_failed += 1;
                continue;
            };
            if !event.relevant || normalize_category(&event.category) == "otp" {
                tally.not_relevant += 1;
                self.mark_processed(user_id, &digest, "irrelevant", None)
                    .await?;
                continue;
            }
            match self
                .store(user_id, batch_id, &message, &digest, event)
                .await?
            {
                Stored::Written(span_id) => {
                    tally.written += 1;
                    self.mark_processed(user_id, &digest, "written", Some(span_id))
                        .await?;
                }
                Stored::Merged(span_id) => {
                    tally.merged += 1;
                    self.mark_processed(user_id, &digest, "merged", Some(span_id))
                        .await?;
                }
            }
        }

        sqlx::query("UPDATE sms_batches SET status = 'processed', processed_at = $1 WHERE id = $2")
            .bind(Utc::now())
            .bind(batch_id)
            .execute(self.db.pool())
            .await?;

        tracing::info!(
            %batch_id, %user_id, total,
            otp_skipped = tally.otp,
            duplicates = tally.duplicate,
            classify_failed = tally.classify_failed,
            not_relevant = tally.not_relevant,
            written = tally.written,
            merged = tally.merged,
            "sms batch processed"
        );

        Ok(())
    }

    async fn store(
        &self,
        user_id: Uuid,
        batch_id: Uuid,
        message: &SmsMessage,
        digest: &str,
        event: ExtractedSmsEvent,
    ) -> Result<Stored, sqlx::Error> {
        let direction = clean(event.direction.as_deref()).map(|d| d.to_lowercase());
        let reported_status = clean(event.status.as_deref()).map(|s| s.to_lowercase());
        let mut category = normalize_category(&event.category);
        if category == "payment" {
            category = if direction.as_deref() == Some("credit") {
                "income".into()
            } else {
                "expense".into()
            };
        }

        let due = event.due_at.as_deref().and_then(parse_when);
        let occurred = event
            .event_at
            .as_deref()
            .and_then(parse_when)
            .unwrap_or(message.received_at);
        let is_due = direction.as_deref() == Some("due")
            && !matches!(reported_status.as_deref(), Some("paid"));
        let (status, start_at, due_at) = if is_due {
            (SpanStatus::Planned, Some(due.unwrap_or(occurred)), due)
        } else {
            (SpanStatus::Done, Some(occurred), None)
        };
        let effective = start_at.unwrap_or(occurred);

        let account_hint = clean(event.account_hint.as_deref()).map(last_digits);
        let reference = clean(event.reference.as_deref()).map(str::to_lowercase);
        let merchant = clean(event.merchant.as_deref());
        let fingerprint = fingerprint(
            is_due,
            reference.as_deref(),
            account_hint.as_deref(),
            merchant,
            event.amount,
            effective,
        );

        if let Some(existing) = self.find_by_fingerprint(user_id, &fingerprint).await? {
            self.merge_duplicate(existing, digest).await?;
            return Ok(Stored::Merged(existing));
        }

        let title = match clean(Some(event.title.as_str())) {
            Some(title) => title.to_string(),
            None => format!("SMS from {}", message.sender),
        };
        let mut data = json!({
            "sender": message.sender,
            "sms_batch_id": batch_id,
            "received_at": message.received_at,
            "fingerprint": fingerprint,
            "direction": direction,
            "reported_status": reported_status,
            "amount": event.amount,
            "currency": event.currency,
            "merchant": merchant,
            "account_hint": account_hint,
            "reference": reference,
            "summary": clean(event.summary.as_deref()),
            "duplicate_count": 0,
        });
        if let Some(attributes) = sanitize_attributes(event.attributes) {
            data["attributes"] = Value::Object(attributes);
        }

        let span_id = SpanRepository::new(self.db.pool().clone())
            .record(
                user_id,
                NewSpan {
                    title,
                    category: Some(category),
                    source: Some("sms".into()),
                    source_ref: Some(digest.to_string()),
                    status: Some(status),
                    start_at,
                    due_at,
                    data: Some(data),
                    ..Default::default()
                },
            )
            .await?;

        if direction.as_deref() == Some("debit")
            && let (Some(account), Some(amount)) = (account_hint.as_deref(), event.amount)
        {
            self.settle_pending_due(user_id, span_id, account, amount)
                .await?;
        }
        Ok(Stored::Written(span_id))
    }

    async fn find_by_fingerprint(
        &self,
        user_id: Uuid,
        fingerprint: &str,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM spans \
             WHERE user_id = $1 AND source = 'sms' AND data->>'fingerprint' = $2 \
             ORDER BY created_at LIMIT 1",
        )
        .bind(user_id)
        .bind(fingerprint)
        .fetch_optional(self.db.pool())
        .await
    }

    async fn merge_duplicate(&self, span_id: Uuid, digest: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE spans SET \
                data = data || jsonb_build_object(\
                    'duplicate_count', COALESCE((data->>'duplicate_count')::int, 0) + 1, \
                    'duplicate_digests', COALESCE(data->'duplicate_digests', '[]'::jsonb) || to_jsonb($2::text)\
                ), \
                version = version + 1, updated_at = now() \
             WHERE id = $1",
        )
        .bind(span_id)
        .bind(digest)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// A payment from the same account for the same amount settles an upcoming
    /// due (an EMI or card bill) that this user's SMS already created.
    async fn settle_pending_due(
        &self,
        user_id: Uuid,
        paid_span: Uuid,
        account_hint: &str,
        amount: f64,
    ) -> Result<(), sqlx::Error> {
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
        .bind(paid_span)
        .bind(account_hint)
        .bind(amount)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    async fn already_processed(&self, user_id: Uuid, digest: &str) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM sms_processed WHERE user_id = $1 AND digest = $2)",
        )
        .bind(user_id)
        .bind(digest)
        .fetch_one(self.db.pool())
        .await
    }

    async fn mark_processed(
        &self,
        user_id: Uuid,
        digest: &str,
        outcome: &str,
        span_id: Option<Uuid>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO sms_processed (user_id, digest, outcome, span_id) \
             VALUES ($1, $2, $3, $4) ON CONFLICT (user_id, digest) DO NOTHING",
        )
        .bind(user_id)
        .bind(digest)
        .bind(outcome)
        .bind(span_id)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    /// The user's own categories first, so the taxonomy grows with their data,
    /// then the seed categories every user starts with.
    async fn known_categories(&self, user_id: Uuid) -> Result<Vec<String>, sqlx::Error> {
        let mut categories: Vec<String> = sqlx::query_scalar::<_, String>(
            "SELECT category FROM spans WHERE user_id = $1 AND source = 'sms' \
             GROUP BY category ORDER BY count(*) DESC LIMIT $2",
        )
        .bind(user_id)
        .bind(MAX_KNOWN_CATEGORIES)
        .fetch_all(self.db.pool())
        .await?;
        for seed in SMS_SEED_CATEGORIES {
            if !categories.iter().any(|known| known == seed) {
                categories.push((*seed).to_string());
            }
        }
        categories.retain(|category| category != "otp");
        Ok(categories)
    }

    /// Prefers a connected local-LLM-capable device for this user (via the
    /// core API's device hub) and falls back to the cloud extractor on any
    /// failure — no device registered, not currently connected, timed out,
    /// or a malformed response. The user never sees the difference; this is
    /// purely a where-it-runs choice.
    async fn classify(
        &self,
        user_id: Uuid,
        message: &SmsMessage,
        known_categories: &[String],
    ) -> Option<ExtractedSmsEvent> {
        if let Some(dispatcher) = &self.device_dispatcher {
            let request = DispatchDeviceRequest {
                user_id,
                capability: "local_llm".to_string(),
                kind: "classify_sms".to_string(),
                params: serde_json::json!({ "sender": message.sender, "body": message.body }),
                timeout_secs: 30,
            };
            if let Ok(value) = dispatcher.dispatch(request).await
                && let Ok(event) = serde_json::from_value::<ExtractedSmsEvent>(value)
            {
                return Some(event);
            }
        }

        self.extractor
            .extract(SmsPrompt {
                sender: message.sender.clone(),
                body: message.body.clone(),
                received_at: message.received_at,
                known_categories: known_categories.to_vec(),
            })
            .await
            .inspect_err(|error| tracing::warn!(%user_id, %error, "sms classification failed"))
            .ok()
    }
}

fn message_digest(message: &SmsMessage) -> String {
    hex::encode(Sha256::digest(format!(
        "{}\n{}\n{}",
        message.sender,
        message.received_at.timestamp_millis(),
        message.body
    )))
}

fn clean(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn last_digits(value: &str) -> String {
    let digits: String = value.chars().filter(char::is_ascii_alphanumeric).collect();
    let skip = digits.len().saturating_sub(4);
    digits[skip..].to_lowercase()
}

/// One real-world event, however many messages describe it: the reference (or
/// account and merchant), the amount, the day it lands on, and whether it is an
/// obligation or money that moved. Category names are left out on purpose,
/// because a model may label the same event differently between messages.
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

/// Accepts `YYYY-MM-DD` (treated as 09:00 in India) or an RFC 3339 timestamp.
fn parse_when(value: &str) -> Option<DateTime<Utc>> {
    let value = value.trim();
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(value) {
        return Some(timestamp.with_timezone(&Utc));
    }
    let date = NaiveDate::parse_from_str(value.get(..10)?, "%Y-%m-%d").ok()?;
    let local = date.and_time(NaiveTime::from_hms_opt(9, 0, 0)?);
    Kolkata
        .from_local_datetime(&local)
        .single()
        .map(|moment| moment.with_timezone(&Utc))
}

fn sanitize_attributes(attributes: Option<Map<String, Value>>) -> Option<Map<String, Value>> {
    let attributes = attributes?;
    let mut kept = Map::new();
    for (key, value) in attributes.into_iter().take(MAX_ATTRIBUTES) {
        let value = match value {
            Value::String(text) => Value::String(text.chars().take(MAX_ATTRIBUTE_TEXT).collect()),
            Value::Number(_) | Value::Bool(_) => value,
            _ => continue,
        };
        kept.insert(key.chars().take(60).collect(), value);
    }
    (!kept.is_empty()).then_some(kept)
}

pub fn looks_like_otp(body: &str) -> bool {
    let lower = body.to_lowercase();
    let mentions_code = lower.contains("otp")
        || lower.contains("verification code")
        || lower.contains("one-time password")
        || lower.contains("one time password")
        || lower.contains("security code");
    if !mentions_code {
        return false;
    }
    body.split(|c: char| !c.is_ascii_digit())
        .any(|token| token.len() >= 4 && token.len() <= 8)
}
