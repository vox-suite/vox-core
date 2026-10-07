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
            .filter_map(|m| {
                let body = sanitize_sms_body(&m.body)?;
                if looks_like_promo(&m.sender, &body) {
                    return None;
                }
                let sanitized = SmsMessage { body, ..m.clone() };
                Some(BatchEvent {
                    source_id: m.sender.clone(),
                    external_event_id: message_digest(&sanitized),
                    occurred_at: m.received_at,
                    payload: json!({ "sender": m.sender, "body": sanitized.body,
                        "authorization_only": looks_like_otp(&m.body) || m.body.contains("OTP [REDACTED]") }),
                })
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

/// Keep financial context, but never upload the authentication code. Unknown
/// code layouts remain excluded rather than guessing which number is secret.
pub fn sanitize_sms_body(body: &str) -> Option<String> {
    if !looks_like_otp(body) {
        return Some(body.to_owned());
    }
    let lower = body.to_lowercase();
    let financial = ["card", "transaction", "txn", "payment", "pay ", "purchase"]
        .iter()
        .any(|s| lower.contains(s));
    let amount = ["inr", "rs.", "rs ", "₹", "usd", "$", "eur", "gbp"]
        .iter()
        .any(|s| lower.contains(s));
    if !financial || !amount {
        return None;
    }
    static CODE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)\b(?:otp|verification code|one-time password|one time password|security code)\s*(?:is\s*|:|=|-)?\s*[0-9]{4,8}\b|\b[0-9]{4,8}\s+(?:is\s+)?(?:your\s+)?(?:otp|verification code|one-time password|one time password|security code)\b").unwrap()
    });
    if !CODE.is_match(body) && !body.contains("OTP [REDACTED]") {
        return None;
    }
    static CONTEXT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:inr|rs\.?|₹|usd|\$|eur|gbp)\s*[0-9][0-9,.]*|(?:card|a/c|acct|account)\s*(?:ending\s*(?:in\s*)?|no\.?\s*)?[*xX -]*[0-9]{4}\b").unwrap()
    });
    static NUMBERS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b[0-9]{4,8}\b").unwrap());
    let safe_ranges: Vec<_> = CODE
        .find_iter(body)
        .chain(CONTEXT.find_iter(body))
        .collect();
    if NUMBERS.find_iter(body).any(|n| {
        !safe_ranges
            .iter()
            .any(|r| r.start() <= n.start() && r.end() >= n.end())
    }) {
        return None;
    }
    Some(CODE.replace_all(body, "OTP [REDACTED]").into_owned())
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

#[cfg(test)]
mod otp_tests {
    use super::*;
    #[test]
    fn transaction_otp_is_retained_without_secret() {
        let body = "OTP is 654321 for transaction of INR 1,250 on credit card XX4321 at AMAZON. Valid for 5 minutes.";
        let sanitized = sanitize_sms_body(body).expect("transaction details retained");
        assert!(!sanitized.contains("654321"));
        assert!(sanitized.contains("1,250"));
        assert!(sanitized.contains("XX4321"));
        assert!(sanitized.contains("AMAZON"));
    }
    #[test]
    fn android_redaction_survives_server_filter() {
        let body = "654321 is your OTP for INR 1200 transaction on card XX4321 at AMAZON";
        let redacted = sanitize_sms_body(body).unwrap();
        assert_eq!(sanitize_sms_body(&redacted), Some(redacted));
    }
    #[test]
    fn additional_unlabelled_secret_is_not_uploaded() {
        assert!(
            sanitize_sms_body("OTP 654321 for INR 1200 on card XX4321. Alternate code 987654")
                .is_none()
        );
    }
    #[test]
    fn pure_and_ambiguous_otps_are_dropped() {
        assert!(sanitize_sms_body("Your login OTP is 654321").is_none());
        assert!(sanitize_sms_body("Use OTP to pay INR 1200 on card 4321. Code: 654321").is_none());
    }
    #[test]
    fn ordinary_transaction_is_unchanged() {
        let body = "INR 1200 spent on card XX4321 at AMAZON";
        assert_eq!(sanitize_sms_body(body).as_deref(), Some(body));
    }
}
