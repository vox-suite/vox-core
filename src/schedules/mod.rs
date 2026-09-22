pub mod handler;
pub mod service;
pub mod ticker;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ScheduleId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScheduleKind {
    Once,
    Recurring,
}

impl ScheduleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Recurring => "recurring",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "once" => Some(Self::Once),
            "recurring" => Some(Self::Recurring),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateScheduleRequest {
    pub instruction: String,
    pub schedule_kind: ScheduleKind,
    pub run_at: Option<DateTime<Utc>>,
    pub recurrence_expression: Option<String>,
    pub timezone: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UpdateScheduleRequest {
    pub state: Option<String>,
    pub run_at: Option<DateTime<Utc>>,
    pub recurrence_expression: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScheduleResponse {
    pub id: ScheduleId,
    pub instruction: String,
    pub schedule_kind: ScheduleKind,
    pub recurrence_expression: Option<String>,
    pub timezone: String,
    pub next_run_at: Option<DateTime<Utc>>,
    pub state: String,
}
