pub mod conversation;
pub mod event_planner;
pub mod summarizer;
pub mod tools;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentError {
    #[error("agent returned invalid structured output")]
    InvalidStructuredOutput,
    #[error("agent provider request failed")]
    Provider,
}

fn structured_json(raw: &str) -> &str {
    let trimmed = raw.trim();
    trimmed
        .strip_prefix("```json")
        .and_then(|value| value.strip_suffix("```"))
        .map(str::trim)
        .unwrap_or(trimmed)
}
