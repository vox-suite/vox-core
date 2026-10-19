/**
* Event planning agent decomposing user intents into structured action plans.
*/
use super::{AgentError, structured_json};
use crate::{config::Config, identity::UserId};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug)]
pub struct EventPlanningPrompt {
    pub user_id: UserId,
    pub user_context: String,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub payload: Value,
}

#[async_trait]
pub trait EventPlanning: Send + Sync {
    async fn plan(&self, prompt: EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError>;
}

pub struct GeminiEventPlanner {
    gemini: Option<gemini::Client>,
    model: String,
    http: reqwest::Client,
    exa_api_key: String,
    google_maps_api_key: Option<String>,
}

impl GeminiEventPlanner {
    pub fn new(config: &Config) -> Result<Self, AgentError> {
        let dependencies = super::tools::dependencies::ToolDependencies::new()
            .map_err(|_| AgentError::Provider)?;
        Ok(Self {
            gemini: gemini::Client::new(&config.gemini_api_key).ok(),
            model: config.gemini_model.clone(),
            http: dependencies.http,
            exa_api_key: config.exa_api_key.clone(),
            google_maps_api_key: config.google_maps_api_key.clone(),
        })
    }
}

#[async_trait]
impl EventPlanning for GeminiEventPlanner {
    async fn plan(&self, prompt: EventPlanningPrompt) -> Result<Vec<PlannedAction>, AgentError> {
        let client = self.gemini.as_ref().ok_or(AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .name("event-planner-agent")
            .record_content_telemetry(crate::telemetry::record_content())
            .preamble("You plan actions for Vox. Assess the event using current tools when needed. Return ONLY one JSON object with exactly these keys, no markdown and no text outside it:\n{\"version\": 1, \"actions\": [{\"kind\": \"outbound_call\", \"reason\": \"why this call is needed now\", \"opening_instruction\": \"what the voice agent should tell the user when they answer\"}]}\nversion is the number 1. outbound_call is the only allowed kind, and each action has only kind, reason and opening_instruction. Return {\"version\": 1, \"actions\": []} when no action is useful.")
            .tool(super::tools::web_search::WebSearch::new(self.http.clone(), self.exa_api_key.clone()))
            .tool(super::tools::google_maps::SearchPlaces::new(self.http.clone(), self.google_maps_api_key.clone()))
            .tool(super::tools::google_maps::GetRoute::new(self.http.clone(), self.google_maps_api_key.clone()))
            .default_max_turns(10)
            .build();
        let input = format!(
            "User context:\n{}\nEvent type: {}\nOccurred at: {}\nPayload: {}",
            prompt.user_context, prompt.event_type, prompt.occurred_at, prompt.payload
        );
        let output = agent
            .prompt(input)
            .extended_details()
            .await
            .map_err(|_| AgentError::Provider)?;
        crate::usage::record_calls(
            Some(prompt.user_id.0),
            "event_planner",
            &self.model,
            &output.completion_calls,
        );
        let output = output.output;
        parse_planned_actions(&output)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PlannedAction {
    OutboundCall {
        reason: String,
        opening_instruction: String,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionPlan {
    version: u8,
    actions: Vec<PlannedAction>,
}

pub fn parse_planned_actions(raw: &str) -> Result<Vec<PlannedAction>, AgentError> {
    let plan: ActionPlan = serde_json::from_str(structured_json(raw))
        .map_err(|_| AgentError::InvalidStructuredOutput)?;
    if plan.version != 1 {
        return Err(AgentError::InvalidStructuredOutput);
    }
    Ok(plan.actions)
}
