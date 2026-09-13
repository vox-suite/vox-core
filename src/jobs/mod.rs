use chrono::{DateTime, Utc};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JobKind {
    ProcessEvent,
    RunSchedule,
    DispatchAction,
    SummarizeConversation,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessEvent => "process_event",
            Self::RunSchedule => "run_schedule",
            Self::DispatchAction => "dispatch_action",
            Self::SummarizeConversation => "summarize_conversation",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_event" => Some(Self::ProcessEvent),
            "run_schedule" => Some(Self::RunSchedule),
            "dispatch_action" => Some(Self::DispatchAction),
            "summarize_conversation" => Some(Self::SummarizeConversation),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ClaimedJob {
    pub id: Uuid,
    pub kind: JobKind,
    pub payload_reference_id: Uuid,
    pub occurrence_at: Option<DateTime<Utc>>,
    pub attempt_count: i32,
}
