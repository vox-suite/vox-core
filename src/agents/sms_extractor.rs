use super::{AgentError, structured_json};
use crate::config::Config;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
pub use vox_shared::sms::ExtractedSmsEvent;

#[derive(Clone, Debug)]
pub struct SmsPrompt {
    pub sender: String,
    pub body: String,
    pub received_at: DateTime<Utc>,
    pub known_categories: Vec<String>,
}

#[async_trait]
pub trait SmsExtracting: Send + Sync {
    async fn extract(&self, prompt: SmsPrompt) -> Result<ExtractedSmsEvent, AgentError>;
}

pub struct GeminiSmsExtractor {
    api_key: String,
    model: String,
}

impl GeminiSmsExtractor {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

fn preamble(known_categories: &[String]) -> String {
    format!(
        "You extract structured facts from one SMS for a personal timeline of money, bills, \
         reminders, deliveries, appointments and travel. Output ONLY one JSON object, with no \
         markdown and no text outside it.\n\
         {{\n\
           \"relevant\": true|false,\n\
           \"category\": \"short snake_case label\",\n\
           \"title\": \"under 80 characters, specific (include merchant or account)\",\n\
           \"summary\": \"one sentence\" or null,\n\
           \"direction\": \"debit\"|\"credit\"|\"due\"|\"info\",\n\
           \"status\": \"paid\"|\"pending\"|\"upcoming\"|\"overdue\"|\"info\",\n\
           \"amount\": number or null,\n\
           \"currency\": \"ISO 4217 code such as INR\" or null,\n\
           \"merchant\": string or null,\n\
           \"account_hint\": \"last 4 digits of the card or account\" or null,\n\
           \"reference\": \"transaction, UPI, bill or order id printed in the message\" or null,\n\
           \"due_at\": \"YYYY-MM-DD, or ISO 8601 with time\" or null,\n\
           \"event_at\": \"when the event happened, if not when the SMS arrived\" or null,\n\
           \"attributes\": {{ any other useful facts as key/value pairs }} or null\n\
         }}\n\
         Rules:\n\
         - Set relevant to false for personal or social chat, marketing and offers with no \
         concrete transaction or obligation, and one-time passwords (use category \"otp\").\n\
         - Capture obligations that are coming up, not only money already moved: an EMI or loan \
         instalment, a credit card bill (total and minimum due), an insurance premium, a rent, \
         subscription or utility bill, a scheduled delivery or appointment. Use direction \"due\" \
         with status \"upcoming\", \"pending\" or \"overdue\", and set due_at from the message. \
         Resolve relative dates such as \"tomorrow\" or \"on the 5th\" against the message date.\n\
         - A payment made is direction \"debit\" and one received is \"credit\", both with \
         status \"paid\".\n\
         - For category, reuse one of the existing categories when it fits: [{}]. Only invent a \
         new short snake_case category when none of them fits.\n\
         - Put extra facts in attributes (for example minimum_due, outstanding_balance, \
         card_network, loan_account, policy_number, tracking_id, balance_after).\n\
         - Never include one-time passwords, PINs, CVVs, passwords or full card numbers anywhere \
         in your response.",
        known_categories.join(", ")
    )
}

#[async_trait]
impl SmsExtracting for GeminiSmsExtractor {
    async fn extract(&self, prompt: SmsPrompt) -> Result<ExtractedSmsEvent, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("sms-extractor-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&preamble(&prompt.known_categories))
            .build();

        let raw = agent
            .prompt(format!(
                "Message date: {}\nSender: {}\nMessage: {}",
                prompt.received_at.format("%Y-%m-%d %H:%M UTC"),
                prompt.sender,
                prompt.body
            ))
            .await
            .map_err(|_| AgentError::Provider)?;

        serde_json::from_str(structured_json(&raw)).map_err(|_| AgentError::InvalidStructuredOutput)
    }
}
