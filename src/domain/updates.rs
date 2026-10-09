use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateKind {
    Briefing,
    EmailNotice,
    ProcessingIssue,
    ConnectionStatus,
    DailyPlan,
}

impl UpdateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Briefing => "briefing",
            Self::EmailNotice => "email_notice",
            Self::ProcessingIssue => "processing_issue",
            Self::ConnectionStatus => "connection_status",
            Self::DailyPlan => "daily_plan",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "briefing" => Some(Self::Briefing),
            "email_notice" => Some(Self::EmailNotice),
            "processing_issue" => Some(Self::ProcessingIssue),
            "connection_status" => Some(Self::ConnectionStatus),
            "daily_plan" => Some(Self::DailyPlan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStatus {
    Active,
    Resolved,
    Dismissed,
}

impl UpdateStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Resolved => "resolved",
            Self::Dismissed => "dismissed",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "active" => Some(Self::Active),
            "resolved" => Some(Self::Resolved),
            "dismissed" => Some(Self::Dismissed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UpdatePriority {
    Low,
    Standard,
    High,
    Urgent,
}

impl UpdatePriority {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Standard => "standard",
            Self::High => "high",
            Self::Urgent => "urgent",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "low" => Some(Self::Low),
            "standard" => Some(Self::Standard),
            "high" => Some(Self::High),
            "urgent" => Some(Self::Urgent),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UpdateItem {
    pub id: Uuid,
    pub user_id: Uuid,
    pub kind: String,
    pub content_version: i32,
    pub category: String,
    pub title: String,
    pub summary: Option<String>,
    pub content: serde_json::Value,
    pub ui_hint: serde_json::Value,
    pub priority: String,
    pub status: String,
    pub read_at: Option<DateTime<Utc>>,
    pub source_job_id: Option<Uuid>,
    pub dedupe_key: Option<String>,
    pub published_at: DateTime<Utc>,
    pub expires_at: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub available_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema)]
pub struct UpdatesQuery {
    pub before: Option<DateTime<Utc>>,
    pub before_id: Option<Uuid>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub category: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema)]
pub struct JobRetryRequest {
    pub idempotency_key: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JobInputRequest {
    pub input_type: String,
    pub data: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct JobActionResponse {
    pub job_id: Uuid,
    pub status: String,
    pub message: String,
}
