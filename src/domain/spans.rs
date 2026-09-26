/**
* Domain model for spans: anything that occupies time, past, present, or planned.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    #[default]
    Planned,
    Active,
    WaitingUser,
    Done,
    Failed,
    Cancelled,
}

impl SpanStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::WaitingUser => "waiting_user",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "planned" => Some(Self::Planned),
            "active" => Some(Self::Active),
            "waiting_user" => Some(Self::WaitingUser),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    Autonomous,
    Interactive,
    ManualHuman,
}

impl ExecutionType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Autonomous => "autonomous",
            Self::Interactive => "interactive",
            Self::ManualHuman => "manual_human",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "autonomous" => Some(Self::Autonomous),
            "interactive" => Some(Self::Interactive),
            "manual_human" => Some(Self::ManualHuman),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    pub id: Uuid,
    pub user_id: Uuid,
    pub parent_id: Option<Uuid>,
    pub title: String,
    pub notes: String,
    pub category: String,
    pub source: String,
    pub source_ref: Option<String>,
    pub status: SpanStatus,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub priority: i32,
    pub execution_type: Option<ExecutionType>,
    pub execution_result: serde_json::Value,
    pub data: serde_json::Value,
    pub collection_ids: Vec<Uuid>,
    pub version: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewSpan {
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub category: Option<String>,
    pub source: Option<String>,
    pub source_ref: Option<String>,
    pub status: Option<SpanStatus>,
    pub parent_id: Option<Uuid>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub priority: Option<i32>,
    pub execution_type: Option<ExecutionType>,
    pub data: Option<serde_json::Value>,
    #[serde(default)]
    pub collection_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpanPatch {
    pub expected_version: Option<i32>,
    pub title: Option<String>,
    pub notes: Option<String>,
    pub category: Option<String>,
    pub status: Option<SpanStatus>,
    #[serde(default, deserialize_with = "present")]
    pub start_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "present")]
    pub end_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "present")]
    pub due_at: Option<Option<DateTime<Utc>>>,
    pub priority: Option<i32>,
    pub execution_result: Option<serde_json::Value>,
    pub data: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct SpanQuery {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub collection_id: Option<Uuid>,
    pub status: Option<SpanStatus>,
    #[serde(default)]
    pub unscheduled: bool,
    pub limit: Option<i64>,
}

pub(crate) fn present<'de, D, T>(d: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}
