use super::{AgentError, structured_json};
use crate::config::Config;
use crate::domain::charts::{ChartType, QuerySpec};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SchemaSample {
    pub id: Uuid,
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub json_schema: Value,
    pub sample_data: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct ChartSuggestionPrompt {
    pub schemas: Vec<SchemaSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChartSuggestion {
    pub title: String,
    pub description: String,
    pub chart_type: String,
    pub schema_ids: Vec<Uuid>,
    pub query_spec: QuerySpec,
}

#[derive(Debug, Clone, Deserialize)]
struct RawChartSuggestion {
    pub title: String,
    pub description: Option<String>,
    pub chart_type: String,
    #[serde(default)]
    pub schema_ids: Vec<Uuid>,
    pub query_spec: QuerySpec,
}

#[async_trait]
pub trait SuggestingCharts: Send + Sync {
    async fn suggest(
        &self,
        prompt: ChartSuggestionPrompt,
    ) -> Result<Vec<ChartSuggestion>, AgentError>;
}

pub struct GeminiChartSuggester {
    api_key: String,
    model: String,
}

impl GeminiChartSuggester {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

fn schema_has_field(schema: &SchemaSample, field: &str) -> bool {
    if let Some(props) = schema.json_schema.get("properties").and_then(|p| p.as_object()) {
        if props.contains_key(field) {
            return true;
        }
    }
    for sample in &schema.sample_data {
        if let Some(obj) = sample.as_object() {
            if obj.contains_key(field) {
                return true;
            }
        }
    }
    false
}

#[async_trait]
impl SuggestingCharts for GeminiChartSuggester {
    async fn suggest(
        &self,
        prompt: ChartSuggestionPrompt,
    ) -> Result<Vec<ChartSuggestion>, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;

        let mut schema_descriptions = Vec::new();
        for s in &prompt.schemas {
            let sample_str = serde_json::to_string(&s.sample_data).unwrap_or_else(|_| "[]".to_string());
            let schema_str = serde_json::to_string(&s.json_schema).unwrap_or_else(|_| "{}".to_string());
            schema_descriptions.push(format!(
                "Category ID: {}\nQualified Name: {}.{}\nDescription: {}\nJSON Schema: {}\nRecent Data Samples: {}\n",
                s.id, s.namespace, s.name, s.description, schema_str, sample_str
            ));
        }
        let categories_context = schema_descriptions.join("\n---\n");

        let preamble_text = format!(
            "You are an analytics assistant. Suggest insightful chart ideas for a user's data categories.\n\
             Output ONLY a valid JSON array of chart suggestions. Do not include markdown code blocks or explanations outside the JSON.\n\
             Each element in the array must be an object with:\n\
             - \"title\": concise name for the chart\n\
             - \"description\": one sentence on what insights this chart reveals\n\
             - \"chart_type\": exactly one of \"line\", \"bar\", \"pie\", \"area\"\n\
             - \"schema_ids\": array of UUID strings of the category used\n\
             - \"query_spec\": object with:\n\
                 - \"metric_field\": a numeric property name that actually exists in the schema properties or data samples\n\
                 - \"aggregation\": exactly one of \"sum\", \"count\", \"avg\", \"min\", \"max\"\n\
                 - \"group_by\": \"day\", \"week\", \"month\", or a categorical field name from the schema\n\
             Rules:\n\
             - metric_field MUST exist in the schema's properties or data.\n\
             - For line and area charts, group_by should usually be \"day\", \"week\", or \"month\".\n\
             - For bar and pie charts, group_by can be categorical or a time bucket.\n\
             - Propose between 3 and 6 relevant and distinct charts."
        );

        let agent = client
            .agent(&self.model)
            .name("chart-suggester-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&preamble_text)
            .build();

        let prompt_msg = format!(
            "Available Categories:\n\n{}\n\nGenerate 3 to 6 chart suggestions for these categories.",
            categories_context
        );

        let raw = agent
            .prompt(prompt_msg)
            .await
            .map_err(|_| AgentError::Provider)?;

        let raw_json = structured_json(&raw);
        let parsed: Vec<RawChartSuggestion> =
            serde_json::from_str(raw_json).map_err(|_| AgentError::InvalidStructuredOutput)?;

        let mut validated = Vec::new();
        for item in parsed {
            let chart_type = match ChartType::parse(&item.chart_type) {
                Some(ct) => ct.as_str().to_string(),
                None => continue,
            };

            let schema_ids = if item.schema_ids.is_empty() {
                prompt.schemas.iter().map(|s| s.id).collect()
            } else {
                item.schema_ids
            };

            let field_exists = prompt.schemas.iter().any(|s| {
                schema_ids.contains(&s.id) && schema_has_field(s, &item.query_spec.metric_field)
            });

            if !field_exists {
                continue;
            }

            validated.push(ChartSuggestion {
                title: item.title,
                description: item.description.unwrap_or_default(),
                chart_type,
                schema_ids,
                query_spec: item.query_spec,
            });
        }

        Ok(validated)
    }
}
