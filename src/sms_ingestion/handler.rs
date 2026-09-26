use chrono::Utc;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    agents::sms_extractor::{ExtractedSmsEvent, SmsExtracting, SmsPrompt},
    core_api_client::{DeviceDispatcher, DispatchDeviceRequest},
    db::Db,
    domain::spans::{NewSpan, SpanStatus},
    sms_ingestion::SmsMessage,
    storage::spans::SpanRepository,
};

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
        let (mut otp_skipped, mut classify_failed, mut not_relevant, mut written) = (0, 0, 0, 0);

        for message in messages {
            if looks_like_otp(&message.body) {
                otp_skipped += 1;
                continue;
            }

            let Some(extracted) = self.classify(user_id, &message).await else {
                classify_failed += 1;
                continue;
            };

            if !extracted.relevant || extracted.category == "otp" {
                not_relevant += 1;
                continue;
            }

            let digest = Sha256::digest(format!(
                "{}\n{}\n{}",
                message.sender,
                message.received_at.timestamp_millis(),
                message.body
            ));
            let category = if extracted.category == "payment" {
                "expense".to_string()
            } else {
                extracted.category
            };
            let title = if extracted.title.trim().is_empty() {
                format!("SMS from {}", message.sender)
            } else {
                extracted.title
            };
            SpanRepository::new(self.db.pool().clone())
                .record(
                    user_id,
                    NewSpan {
                        title,
                        category: Some(category),
                        source: Some("sms".into()),
                        source_ref: Some(hex::encode(digest)),
                        status: Some(SpanStatus::Done),
                        start_at: Some(message.received_at),
                        data: Some(serde_json::json!({
                            "sender": message.sender,
                            "amount": extracted.amount,
                            "currency": extracted.currency,
                            "sms_batch_id": batch_id,
                        })),
                        ..Default::default()
                    },
                )
                .await?;
            written += 1;
        }

        sqlx::query("UPDATE sms_batches SET status = 'processed', processed_at = $1 WHERE id = $2")
            .bind(Utc::now())
            .bind(batch_id)
            .execute(self.db.pool())
            .await?;

        tracing::info!(
            %batch_id, %user_id, total, otp_skipped, classify_failed, not_relevant, written,
            "sms batch processed"
        );

        Ok(())
    }

    /// Prefers a connected local-LLM-capable device for this user (via the
    /// core API's device hub) and falls back to the cloud extractor on any
    /// failure — no device registered, not currently connected, timed out,
    /// or a malformed response. The user never sees the difference; this is
    /// purely a where-it-runs choice.
    async fn classify(&self, user_id: Uuid, message: &SmsMessage) -> Option<ExtractedSmsEvent> {
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
            })
            .await
            .inspect_err(|error| tracing::warn!(%user_id, %error, "sms classification failed"))
            .ok()
    }
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
