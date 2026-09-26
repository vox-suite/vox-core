/**
* Conversation summarization agent condensing dialogue history.
*/
use super::{AgentError, conversation::PromptMessage, gateway, structured_json};
use crate::{config::Config, summaries::StructuredSummary};
use async_trait::async_trait;

const PREAMBLE: &str = "You are an expert conversation summarizer for Vox. Analyze the provided conversation \
     and output ONLY a valid JSON object with the following schema:\n\
     {\n\
       \"recap\": \"concise summary of the interaction\",\n\
       \"profile_updates\": { \"key\": \"value\" },\n\
       \"commitments\": [\"any action items or followups committed by user or assistant\"],\n\
       \"decisions\": [\"key decisions reached\"]\n\
     }\n\
     Do not include any extra text or markdown outside of the JSON.";

#[derive(Clone, Debug)]
pub struct SummaryPrompt {
    pub messages: Vec<PromptMessage>,
}

#[async_trait]
pub trait Summarizing: Send + Sync {
    async fn summarize(&self, prompt: SummaryPrompt) -> Result<StructuredSummary, AgentError>;
}

pub struct GeminiSummarizer {
    api_key: String,
    model: String,
    gateway: Option<gateway::LlmGateway>,
}

impl GeminiSummarizer {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
            gateway: config.llm_gateway(),
        }
    }
}

#[async_trait]
impl Summarizing for GeminiSummarizer {
    async fn summarize(&self, prompt: SummaryPrompt) -> Result<StructuredSummary, AgentError> {
        let mut transcript = String::new();
        for msg in prompt.messages {
            transcript.push_str(&format!("{}: {}\n", msg.role, msg.text));
        }

        let raw = gateway::complete(
            &self.api_key,
            &self.model,
            self.gateway.as_ref(),
            PREAMBLE,
            format!("Summarize this conversation:\n{transcript}"),
        )
        .await?;

        parse_summary(&raw)
    }
}

pub fn parse_summary(raw: &str) -> Result<StructuredSummary, AgentError> {
    serde_json::from_str(structured_json(raw)).map_err(|_| AgentError::InvalidStructuredOutput)
}
