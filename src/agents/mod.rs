/**
* Agent subsystem root coordinating LLM orchestration and tools.
*/
pub mod chart_suggester;
pub mod conversation;
pub mod event_agent;
pub mod event_planner;
pub mod prompts;
pub mod schema_extractor;
pub mod space_architect;
pub mod space_runtime;
pub mod summarizer;
pub mod tools;
pub mod utils;

pub use chart_suggester::*;
pub use space_architect::*;
pub use space_runtime::*;
pub use utils::structured_json;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AgentError {
    #[error("agent returned invalid structured output")]
    InvalidStructuredOutput,
    #[error("agent provider request failed")]
    Provider,
}
