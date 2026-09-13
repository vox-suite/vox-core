use super::{AgentError, structured_json};
use serde::{Deserialize, Serialize};

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
