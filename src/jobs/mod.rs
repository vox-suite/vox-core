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
    EvaluateTask,
    ExecuteTask,
    ProcessSmsBatch,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProcessEvent => "process_event",
            Self::RunSchedule => "run_schedule",
            Self::SummarizeConversation => "summarize_conversation",
            Self::EvaluateTask => "evaluate_task",
            Self::ExecuteTask => "execute_task",
            Self::ProcessSmsBatch => "process_sms_batch",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process_event" => Some(Self::ProcessEvent),
            "run_schedule" => Some(Self::RunSchedule),
            "summarize_conversation" => Some(Self::SummarizeConversation),
            "evaluate_task" => Some(Self::EvaluateTask),
            "execute_task" => Some(Self::ExecuteTask),
            "process_sms_batch" => Some(Self::ProcessSmsBatch),
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
