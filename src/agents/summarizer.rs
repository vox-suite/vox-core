use super::{AgentError, structured_json};
use crate::summaries::StructuredSummary;

pub fn parse_summary(raw: &str) -> Result<StructuredSummary, AgentError> {
    serde_json::from_str(structured_json(raw)).map_err(|_| AgentError::InvalidStructuredOutput)
}
