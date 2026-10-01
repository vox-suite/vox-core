use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    consent::{ConsentError, ConsentService, DataSource},
    db::Db,
    events::{
        EventId,
        service::{BatchEvent, EventService},
    },
};

const MAX_BATCH_MESSAGES: usize = 512;

pub mod finance;
pub mod retention;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SmsMessage {
    pub sender: String,
    pub body: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SmsIngestionError {
    #[error("batch must contain at least one message")]
    Empty,
    #[error("batch exceeds the maximum number of messages")]
    TooLarge,
    #[error("sms data sharing consent has not been granted")]
    ConsentRequired,
    #[error("sms batch storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("event ingestion unavailable")]
    Event(#[from] crate::events::EventError),
    #[error("consent storage unavailable")]
    Consent(#[from] ConsentError),
}

#[derive(Clone)]
pub struct SmsIngestionService {
    events: EventService,
    consent: ConsentService,
}

impl SmsIngestionService {
    pub fn new(db: Db) -> Self {
        let events = EventService::new(db.clone());
        let consent = ConsentService::new(db);
        Self { events, consent }
    }

    /// Stores the batch with a bulk insert and advances the server sync cursor, then
    /// returns the new ids and the cursor. Processing (triage, span extraction) runs
    /// later in the worker, so the caller gets an answer in a couple of round trips.
    pub async fn submit_batch(
        &self,
        user_id: Uuid,
        messages: Vec<SmsMessage>,
    ) -> Result<SmsBatchResult, SmsIngestionError> {
        if messages.is_empty() {
            return Err(SmsIngestionError::Empty);
        }
        if messages.len() > MAX_BATCH_MESSAGES {
            return Err(SmsIngestionError::TooLarge);
        }
        if !self.consent.is_granted(user_id, DataSource::Sms).await? {
            return Err(SmsIngestionError::ConsentRequired);
        }

        let items: Vec<BatchEvent> = messages
            .iter()
            .filter(|m| !looks_like_otp(&m.body) && !looks_like_promo(&m.sender, &m.body))
            .map(|m| BatchEvent {
                source_id: m.sender.clone(),
                external_event_id: message_digest(m),
                occurred_at: m.received_at,
                payload: json!({ "sender": m.sender, "body": m.body }),
            })
            .collect();
        let event_ids = self
            .events
            .ingest_batch_for_user(user_id, "sms", "sms_message", &items)
            .await?;

        let newest_received_at = messages.iter().map(|m| m.received_at).max().unwrap();
        self.consent
            .advance_sync_cursor(user_id, DataSource::Sms, newest_received_at)
            .await?;

        Ok(SmsBatchResult {
            event_ids,
            synced_until: newest_received_at,
        })
    }
}

pub struct SmsBatchResult {
    pub event_ids: Vec<EventId>,
    pub synced_until: DateTime<Utc>,
}

fn message_digest(message: &SmsMessage) -> String {
    hex::encode(Sha256::digest(format!(
        "{}\n{}\n{}",
        message.sender,
        message.received_at.timestamp_millis(),
        message.body
    )))
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

const PROMO_MARKERS: &[&str] = &[
    "unsubscribe",
    "t&c apply",
    "tnc apply",
    "terms apply",
    "% off",
    "flat off",
    "mega sale",
    "limited time",
    "use code",
    "coupon",
    "click here",
    "shop now",
    "visit now",
    "reply stop",
    "sms stop",
    "opt out",
];

const MONEY_MARKERS: &[&str] = &[
    "debited",
    "credited",
    "spent",
    "paid",
    "payment",
    "due",
    "emi",
    "statement",
    "a/c",
    "acct",
    "txn",
    "transaction",
    "upi",
    "refund",
    "balance",
    "bill",
    "invoice",
    "rs.",
    "rs ",
    "inr",
    "\u{20b9}",
    "usd",
    "$",
];

pub fn looks_like_promo(sender: &str, body: &str) -> bool {
    let lower = body.to_lowercase();
    let promotional = sender.trim().to_ascii_uppercase().ends_with("-P")
        || PROMO_MARKERS.iter().any(|marker| lower.contains(marker));
    promotional && !MONEY_MARKERS.iter().any(|marker| lower.contains(marker))
}
