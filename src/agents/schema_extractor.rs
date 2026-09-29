use super::{AgentError, structured_json};
use crate::config::Config;
use crate::jev::schema_classifier::SchemaDescriptor;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct SchemaExtractionPrompt {
    pub event_type: String,
    pub payload: Value,
    pub occurred_at: DateTime<Utc>,
    pub near_miss_schemas: Vec<SchemaDescriptor>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SchemaExtractionResult {
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub json_schema: Value,
    pub data: Value,
    pub title: String,
    pub color_token: i32,
    pub icon_token: i32,
}

#[async_trait]
pub trait SchemaExtracting: Send + Sync {
    async fn extract(
        &self,
        prompt: SchemaExtractionPrompt,
    ) -> Result<SchemaExtractionResult, AgentError>;
}

pub struct GeminiSchemaExtractor {
    api_key: String,
    model: String,
}

impl GeminiSchemaExtractor {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

fn preamble(near_miss: &[SchemaDescriptor]) -> String {
    let near_miss_text = if near_miss.is_empty() {
        "none".to_string()
    } else {
        near_miss
            .iter()
            .map(|s| format!("- {} ({})", s.qualified_name, s.description))
            .collect::<Vec<_>>()
            .join("\n")
    };
    format!(
        "You register a new structured data category for a personal timeline and extract one \
         event into it. Output ONLY one JSON object, with no markdown and no text outside it.\n\
         {{\n\
           \"namespace\": \"high-level domain, e.g. finance, location, health, activity\",\n\
           \"name\": \"specific entity name, e.g. expense, fuel_log, blood_pressure\",\n\
           \"description\": \"clear semantic description of this category and when to use it\",\n\
           \"json_schema\": {{ standard JSON Schema object with \"properties\", \"required\", \
         and field types (string, number, boolean, array, object) }},\n\
           \"data\": {{ the extracted fields for this one event, matching json_schema.properties }},\n\
           \"title\": \"under 80 characters, specific headline for this one event\",\n\
           \"color_token\": integer 0-23, a display color slot for this category,\n\
           \"icon_token\": integer 0-23, a display icon slot for this category\n\
         }}\n\
         Rules:\n\
         - Do not invent a near-duplicate of an existing category. Categories that were already \
         considered and rejected as not matching this event: [{near_miss_text}]. If this event is \
         actually one of those, reuse its namespace and name exactly rather than creating a new one.\n\
         - namespace and name are short snake_case.\n\
         - color_token and icon_token are opaque slot numbers only, not real colors or icons — \
         pick ones that feel distinct from the near-miss categories above so similar categories \
         don't look identical; the actual color/icon each number maps to is decided elsewhere.\n\
         - Never include one-time passwords, PINs, CVVs, passwords, or full card numbers in data."
    )
}

#[async_trait]
impl SchemaExtracting for GeminiSchemaExtractor {
    async fn extract(
        &self,
        prompt: SchemaExtractionPrompt,
    ) -> Result<SchemaExtractionResult, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("schema-extractor-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&preamble(&prompt.near_miss_schemas))
            .build();

        let raw = agent
            .prompt(format!(
                "Event type: {}\nOccurred at: {}\nPayload: {}",
                prompt.event_type,
                prompt.occurred_at.format("%Y-%m-%d %H:%M UTC"),
                prompt.payload,
            ))
            .await
            .map_err(|_| AgentError::Provider)?;

        let mut result: SchemaExtractionResult =
            serde_json::from_str(structured_json(&raw)).map_err(|_| AgentError::InvalidStructuredOutput)?;
        result.color_token = result.color_token.clamp(0, 23);
        result.icon_token = result.icon_token.clamp(0, 23);
        Ok(result)
    }
}
