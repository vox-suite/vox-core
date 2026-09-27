use super::{AgentError, structured_json};
use crate::config::Config;
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
pub use vox_shared::sms::ExtractedSmsEvent;

#[derive(Clone, Debug)]
pub struct SmsPrompt {
    pub sender: String,
    pub body: String,
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

#[async_trait]
impl SmsExtracting for GeminiSmsExtractor {
    async fn extract(&self, prompt: SmsPrompt) -> Result<ExtractedSmsEvent, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("sms-extractor-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&format!(
                "You classify a single SMS message for a personal activity timeline. \
                 Output ONLY a valid JSON object with this schema:\n\
                 {{\n\
                   \"relevant\": true|false,\n\
                   \"category\": {},\n\
                   \"title\": \"short human-readable title, under 80 characters\",\n\
                   \"amount\": number or null (money spent or received, only for payments),\n\
                   \"currency\": \"ISO 4217 code like INR\" or null\n\
                 }}\n\
                 Set relevant to false for personal/social messages, spam, or anything with no \
                 concrete real-world activity to log. Set category to \"otp\" for any one-time \
                 password, verification code, or security code message, and NEVER include the \
                 actual code digits anywhere in your response. Do not include any extra text or \
                 markdown outside of the JSON.",
                vox_shared::sms::SMS_CATEGORIES_PROMPT
            ))
            .build();

        let raw = agent
            .prompt(format!(
                "Sender: {}\nMessage: {}",
                prompt.sender, prompt.body
            ))
            .await
            .map_err(|_| AgentError::Provider)?;

        serde_json::from_str(structured_json(&raw)).map_err(|_| AgentError::InvalidStructuredOutput)
    }
}
