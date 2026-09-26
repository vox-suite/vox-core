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
    api_key: String,
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
            api_key: config.gemini_api_key.clone(),
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
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        let agent = client
            .agent(&self.model)
            .preamble("You plan actions for Vox. Assess the event using current tools when needed. Return only JSON with version 1 and an actions array. The only allowed action kind is outbound_call with reason and opening_instruction. Return an empty actions array when no action is useful.")
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
            .await
            .map_err(|_| AgentError::Provider)?;
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
