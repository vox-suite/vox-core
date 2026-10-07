use super::{AgentError, structured_json};
use crate::config::Config;
use crate::domain::schemas::DataSchema;
use crate::domain::spaces::{AgentSpec, AgentSpecLimits};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};

#[async_trait]
pub trait SpaceArchitecting: Send + Sync {
    async fn generate_spec(
        &self,
        intent: &str,
        available_schemas: &[DataSchema],
    ) -> Result<AgentSpec, AgentError>;
}

pub struct GeminiSpaceArchitect {
    api_key: String,
    model: String,
}

impl GeminiSpaceArchitect {
    pub fn new(config: &Config) -> Self {
        Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
        }
    }
}

fn build_preamble(available_schemas: &[DataSchema]) -> String {
    let schema_catalog = if available_schemas.is_empty() {
        "None available".to_string()
    } else {
        available_schemas
            .iter()
            .map(|s| {
                format!(
                    "- {}.{} (ID: {}): {}",
                    s.namespace, s.name, s.id, s.description
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    format!(
        "You are an AI Architect for Vox Spaces (an ideation system that maps user visions before committing to action).\n\
         Given a user's vision/intent and their existing timeline data categories, produce a tailored agent specification.\n\
         Output ONLY a valid JSON object, with no markdown fences, matching this structure:\n\
         {{\n\
           \"title\": \"A short, specific title of 2-5 words for this space\",\n\
           \"mission\": \"Clear, concise mission statement of what to explore and achieve\",\n\
           \"look_for\": [\n\
             \"Specific data to query or check from user categories (e.g. recent expenses, budget, past trips)\",\n\
             \"External research topics (e.g. crowd-free destinations within 300km of Bangalore)\",\n\
             \"Constraints and trade-offs to evaluate\"\n\
           ],\n\
           \"done_when\": \"Criteria when the space has reached a solid plan or decision\",\n\
           \"limits\": {{\n\
             \"max_steps\": 20,\n\
             \"max_children\": 5\n\
           }}\n\
         }}\n\
         Available user data categories:\n\
         {schema_catalog}"
    )
}

#[async_trait]
impl SpaceArchitecting for GeminiSpaceArchitect {
    async fn generate_spec(
        &self,
        intent: &str,
        available_schemas: &[DataSchema],
    ) -> Result<AgentSpec, AgentError> {
        let Ok(client) = gemini::Client::new(&self.api_key) else {
            return Ok(fallback_spec(intent));
        };

        let agent = client
            .agent(&self.model)
            .name("space-architect")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble(&build_preamble(available_schemas))
            .build();

        let prompt = format!("User intent / vision:\n{}", intent);
        let raw = match agent.prompt(prompt).await {
            Ok(output) => output,
            Err(_) => return Ok(fallback_spec(intent)),
        };

        let json_text = structured_json(&raw);
        match serde_json::from_str::<AgentSpec>(json_text) {
            Ok(mut spec) => {
                if spec.title.trim().is_empty() {
                    spec.title = fallback_title(intent);
                }
                if spec.limits.max_steps == 0 {
                    spec.limits.max_steps = crate::config::DEFAULT_SPACE_MAX_STEPS;
                }
                if spec.limits.max_children == 0 {
                    spec.limits.max_children = crate::config::DEFAULT_SPACE_MAX_CHILDREN;
                }
                Ok(spec)
            }
            Err(_) => Ok(fallback_spec(intent)),
        }
    }
}

fn fallback_title(intent: &str) -> String {
    let words: Vec<&str> = intent.split_whitespace().take(5).collect();
    words.join(" ")
}

fn fallback_spec(intent: &str) -> AgentSpec {
    AgentSpec {
        title: fallback_title(intent),
        mission: format!("Explore and plan: {}", intent),
        look_for: vec![
            "User's past spending and budget headroom".to_string(),
            "Schedule and availability conflicts".to_string(),
            "Candidate options and alternatives".to_string(),
        ],
        done_when: "A concrete comparison and executable plan is formulated.".to_string(),
        limits: AgentSpecLimits {
            max_steps: crate::config::DEFAULT_SPACE_MAX_STEPS,
            max_children: crate::config::DEFAULT_SPACE_MAX_CHILDREN,
        },
    }
}
