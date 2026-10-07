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
                        "authorization_only": looks_like_otp(&m.body) || m.body.contains(REDACTED) }),
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

/// A card, bank or UPI payment that already happened. Due notices, failures, refunds and
/// reminders are excluded so they still go through normal triage.
pub fn looks_like_completed_payment(body: &str) -> bool {
    let lower = body.to_lowercase();
    let amount = ["inr", "rs.", "rs ", "₹", "usd", "$"]
        .iter()
        .any(|s| lower.contains(s));
    let done = [
        "spent",
        "debited",
        "paid ",
        "charged",
        "purchase of",
        "payment of",
        "txn of",
        "transaction of",
        "successfully paid",
    ]
    .iter()
    .any(|s| lower.contains(s));
    let not_a_spend = [
        "due",
        "overdue",
        "will be",
        "failed",
        "declined",
        "unsuccessful",
        "reminder",
        "upcoming",
        "pay by",
        "outstanding",
        "reversed",
        "refund",
        "credited",
        "otp",
        "verification code",
        "pending",
    ]
    .iter()
    .any(|s| lower.contains(s));
    amount && done && !not_a_spend
}

const REDACTED: &str = "[REDACTED]";

/// Replaces only the digits of any labelled authentication code, keeping the rest of the message.
/// Returns `None` when no labelled code was found.
fn redact_codes(body: &str) -> Option<String> {
    const LABEL: &str =
        r"(?:otp|verification code|one-time password|one time password|security code)";
    static DIRECT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)\b{LABEL}\s*(?:is\s*|:|=|-)?\s*([0-9]{{4,8}})\b"
        ))
        .unwrap()
    });
    static CODE_FIRST: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)\b([0-9]{{4,8}})\s+(?:is\s+|as\s+)?(?:(?:your|the|this)\s+)?{LABEL}\b"
        ))
        .unwrap()
    });
    // "Your OTP for txn of INR 500 at AMAZON on card XX1234 is 123456": code at the end of the sentence.
    static TRAILING: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(&format!(
            r"(?i)\b{LABEL}\b[^\n]{{0,160}}?\bis\s*([0-9]{{4,8}})\b"
        ))
        .unwrap()
    });
    fn replace(re: &regex::Regex, text: &str) -> (String, bool) {
        let mut out = String::with_capacity(text.len());
        let mut last = 0;
        let mut found = false;
        for caps in re.captures_iter(text) {
            let code = caps.get(1).unwrap();
            out.push_str(&text[last..code.start()]);
            out.push_str(REDACTED);
            last = code.end();
            found = true;
        }
        out.push_str(&text[last..]);
        (out, found)
    }
    let mut text = body.to_owned();
    let mut any = false;
    for re in [&*DIRECT, &*CODE_FIRST] {
        let (next, found) = replace(re, &text);
        text = next;
        any |= found;
    }
    if !any {
        let (next, found) = replace(&TRAILING, &text);
        text = next;
        any = found;
    }
    any.then_some(text)
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
    // Already redacted on the phone: keep it, but still verify nothing else is secret.
    let cleaned = match redact_codes(body) {
        Some(text) => text,
        None if body.contains(REDACTED) => body.to_owned(),
        None => return None,
    };
    static CONTEXT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"(?i)(?:inr|rs\.?|₹|usd|\$|eur|gbp)\s*[0-9][0-9,.]*|(?:card|a/c|acct|account)\s*(?:ending\s*(?:in\s*)?|no\.?\s*)?[*xX -]*[0-9]{4}\b").unwrap()
    });
    static NUMBERS: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\b[0-9]{4,8}\b").unwrap());
    let safe_ranges: Vec<_> = CONTEXT.find_iter(&cleaned).collect();
    if NUMBERS.find_iter(&cleaned).any(|n| {
        !safe_ranges
            .iter()
            .any(|r| r.start() <= n.start() && r.end() >= n.end())
    }) {
        return None;
    }
    Some(cleaned)
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
    fn completed_payments_are_recognised_and_reminders_are_not() {
        assert!(looks_like_completed_payment(
            "Spent Rs.999 on HDFC Bank Card 9313 at IGP on 07-10-26"
        ));
        assert!(looks_like_completed_payment(
            "INR 1200 debited from a/c XX4321 at AMAZON"
        ));
        assert!(!looks_like_completed_payment(
            "Rs.3500 is due on 10-10-26 for your card"
        ));
        assert!(!looks_like_completed_payment(
            "Payment of INR 500 failed. Please retry"
        ));
        assert!(!looks_like_completed_payment(
            "OTP 123456 for payment of INR 500"
        ));
        assert!(!looks_like_completed_payment(
            "INR 500 credited to your account"
        ));
    }
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
    fn common_bank_layouts_keep_details_and_hide_the_code() {
        for body in [
            "123456 is the OTP for your transaction of INR 2,500.00 at AMAZON on card ending 4321.",
            "Your OTP for txn of INR 2500.00 at AMAZON on HDFC Bank Card ending 4321 is 123456. Do not share.",
            "Use 123456 as OTP to pay INR 2500 on card XX4321 at AMAZON.",
        ] {
            let out = sanitize_sms_body(body).unwrap_or_else(|| panic!("dropped: {body}"));
            assert!(!out.contains("123456"), "{out}");
            assert!(out.contains("AMAZON") && out.contains("4321"), "{out}");
            assert!(out.contains(REDACTED), "{out}");
            assert_eq!(sanitize_sms_body(&out), Some(out.clone()));
        }
    }
    #[test]
    fn ordinary_transaction_is_unchanged() {
        let body = "INR 1200 spent on card XX4321 at AMAZON";
        assert_eq!(sanitize_sms_body(body).as_deref(), Some(body));
    }
}
