/**
* Background job queue management and scheduler implementation.
*/
use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    ProcessEvent,
    RunSchedule,
    SummarizeConversation,
    EvaluateSpan,
    ExecuteSpan,
    ProcessSmsBatch,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessEvent => "process_event",
            Self::RunSchedule => "run_schedule",
            Self::SummarizeConversation => "summarize_conversation",
            Self::EvaluateSpan => "evaluate_span",
            Self::ExecuteSpan => "execute_span",
            Self::ProcessSmsBatch => "process_sms_batch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_event" => Some(Self::ProcessEvent),
            "run_schedule" => Some(Self::RunSchedule),
            "summarize_conversation" => Some(Self::SummarizeConversation),
            "evaluate_span" => Some(Self::EvaluateSpan),
            "execute_span" => Some(Self::ExecuteSpan),
            "process_sms_batch" => Some(Self::ProcessSmsBatch),
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
}
