/**
* Conversation summarization agent condensing dialogue history.
*/
use super::{AgentError, conversation::PromptMessage, structured_json};
use crate::{config::Config, summaries::StructuredSummary};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};

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
}

impl GeminiSummarizer {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

#[async_trait]
impl Summarizing for GeminiSummarizer {
    async fn summarize(&self, prompt: SummaryPrompt) -> Result<StructuredSummary, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("summarizer-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(
                "You are an expert conversation summarizer for Vox. Analyze the provided conversation \
                 and output ONLY a valid JSON object with the following schema:\n\
                 {\n\
                   \"recap\": \"concise summary of the interaction\",\n\
                   \"profile_updates\": { \"key\": \"value\" },\n\
                   \"commitments\": [\"any action items or followups committed by user or assistant\"],\n\
                   \"decisions\": [\"key decisions reached\"]\n\
                 }\n\
                 Do not include any extra text or markdown outside of the JSON.",
            )
            .build();

        let mut transcript = String::new();
        for msg in prompt.messages {
            transcript.push_str(&format!("{}: {}\n", msg.role, msg.text));
        }

        let raw = agent
            .prompt(format!("Summarize this conversation:\n{transcript}"))
            .await
            .map_err(|_| AgentError::Provider)?;

        parse_summary(&raw)
    }
}

pub fn parse_summary(raw: &str) -> Result<StructuredSummary, AgentError> {
    serde_json::from_str(structured_json(raw)).map_err(|_| AgentError::InvalidStructuredOutput)
}
