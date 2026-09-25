use chrono::Utc;
use sqlx::Row;
use std::sync::Arc;
use uuid::Uuid;

use crate::{
    agents::sms_extractor::{SmsExtracting, SmsPrompt},
    db::Db,
    sms_ingestion::SmsMessage,
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
}

impl SmsBatchHandler {
    pub fn new(db: Db, extractor: Arc<dyn SmsExtracting>) -> Self {
        Self { db, extractor }
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

        for message in messages {
            if looks_like_otp(&message.body) {
                continue;
            }

            let extracted = match self
                .extractor
                .extract(SmsPrompt {
                    sender: message.sender.clone(),
                    body: message.body.clone(),
                })
                .await
            {
                Ok(event) => event,
                Err(_) => continue,
            };

            if !extracted.relevant || extracted.category == "otp" {
                continue;
            }

            sqlx::query(
                "INSERT INTO device_timeline_entries (user_id, source, category, title, kind, start_at, sms_batch_id) \
                 VALUES ($1, 'sms', $2, $3, 'completed', $4, $5)",
            )
            .bind(user_id)
            .bind(&extracted.category)
            .bind(&extracted.title)
            .bind(message.received_at)
            .bind(batch_id)
            .execute(self.db.pool())
            .await?;
        }

        sqlx::query("UPDATE sms_batches SET status = 'processed', processed_at = $1 WHERE id = $2")
            .bind(Utc::now())
            .bind(batch_id)
            .execute(self.db.pool())
            .await?;

        Ok(())
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
