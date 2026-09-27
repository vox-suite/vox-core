/**
* Fast tool routing and intent matching for agent queries.
*/
use super::{JevError, client::JevClient};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolDomain {
    None,
    WebSearch,
    Maps,
    TasksAndRecords,
    Calendar,
    Device,
    All,
}

#[derive(Clone)]
pub struct ToolRouter {
    jev: JevClient,
}

impl ToolRouter {
    pub fn new(jev: JevClient) -> Self {
        Self { jev }
    }

    pub async fn classify(&self, prompt: &str) -> Result<(ToolDomain, f64), JevError> {
        let state = json!({ "user_prompt": prompt });
        let instructions = "Select the single external capability required to satisfy the user's request, or 'none' if it can be answered conversationally.";
        let options = &[
            (
                "none",
                Some(
                    "Pure conversation, clarification, smalltalk, greetings, goodbyes, or questions answerable from context",
                ),
            ),
            (
                "web_search",
                Some(
                    "Live news, stock prices, sports scores, weather, general web search, or live facts",
                ),
            ),
            (
                "maps",
                Some("Places, cafes, restaurants, driving duration, distance, traffic, or routes"),
            ),
            (
                "tasks_and_records",
                Some(
                    "Creating or checking tasks, reminders, scheduling outbound phone calls, initiating calls, logging notes, updating facts, or personal records",
                ),
            ),
            (
                "calendar",
                Some("Checking upcoming schedule, calendar events, meetings, or availability"),
            ),
            (
                "device",
                Some(
                    "Opening a terminal, running a shell command, or checking status on the user's registered computer or device (e.g. their Mac, laptop)",
                ),
            ),
            (
                "all",
                Some("Complex or multi-intent request requiring a combination of multiple tools"),
            ),
        ];

        let (choice, confidence, _) = self.jev.choice(state, instructions, options).await?;

        let domain = match choice.as_str() {
            "none" => ToolDomain::None,
            "web_search" => ToolDomain::WebSearch,
            "maps" => ToolDomain::Maps,
            "tasks_and_records" => ToolDomain::TasksAndRecords,
            "calendar" => ToolDomain::Calendar,
            "device" => ToolDomain::Device,
            _ => ToolDomain::All,
        };

        tracing::info!(
            prompt = %prompt,
            domain = ?domain,
            confidence = confidence,
            "Jev System 1: tool domain routed"
        );

        Ok((domain, confidence))
    }

    pub async fn route(&self, prompt: &str) -> ToolDomain {
        match self.classify(prompt).await {
            Ok((domain, _)) => domain,
            Err(_) => ToolDomain::All,
        }
    }
}
