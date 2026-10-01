/**
* Background job queue management and scheduler implementation.
*/
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    ProcessEvent,
    ProcessEventBatch,
    RunSchedule,
    SummarizeConversation,
    EvaluateSpan,
    ExecuteSpan,
    RunSpace,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessEvent => "process_event",
            Self::ProcessEventBatch => "process_event_batch",
            Self::RunSchedule => "run_schedule",
            Self::SummarizeConversation => "summarize_conversation",
            Self::EvaluateSpan => "evaluate_span",
            Self::ExecuteSpan => "execute_span",
            Self::RunSpace => "run_space",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_event" => Some(Self::ProcessEvent),
            "process_event_batch" => Some(Self::ProcessEventBatch),
            "run_schedule" => Some(Self::RunSchedule),
            "summarize_conversation" => Some(Self::SummarizeConversation),
            "evaluate_span" => Some(Self::EvaluateSpan),
            "execute_span" => Some(Self::ExecuteSpan),
            "run_space" => Some(Self::RunSpace),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub payload_reference_id: Option<Uuid>,
    pub occurrence_at: Option<DateTime<Utc>>,
    pub attempt_count: i32,
    pub max_attempts: i32,
}
