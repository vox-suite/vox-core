use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineGroup {
    pub id: Uuid,
    pub value: String,
    pub label: String,
    pub ui_hint: serde_json::Value,
    pub sort_order: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineEventType {
    pub id: Uuid,
    pub owner_user_id: Option<Uuid>,
    pub value: String,
    pub version: i32,
    pub label: String,
    pub group_id: Uuid,
    pub description: String,
    pub content_schema: serde_json::Value,
    pub analytics_definition: serde_json::Value,
    pub ui_hint: serde_json::Value,
    pub state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewEventType {
    pub value: String,
    pub label: String,
    pub group_id: Uuid,
    #[serde(default)]
    pub description: String,
    pub content_schema: serde_json::Value,
    #[serde(default = "empty_metadata")]
    pub analytics_definition: serde_json::Value,
    #[serde(default = "empty_metadata")]
    pub ui_hint: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineEvent {
    pub id: Uuid,
    pub user_id: Uuid,
    pub event_type_id: Uuid,
    pub group_id: Uuid,
    pub title: String,
    pub summary: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub time_precision: String,
    pub source_timezone: Option<String>,
    pub content: serde_json::Value,
    pub record_state: String,
    pub confidence: f64,
    pub dedupe_key: Option<String>,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineEvidence {
    pub id: Uuid,
    pub timeline_event_id: Uuid,
    pub user_id: Uuid,
    pub source_record_id: Option<Uuid>,
    pub source_attachment_id: Option<Uuid>,
    pub source_type: String,
    pub source_id: Option<String>,
    pub raw_reference: Option<String>,
    pub observation_metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineEventWithEvidence {
    pub event: TimelineEvent,
    pub evidence: Vec<TimelineEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineQuery {
    pub spending_only: Option<bool>,
    pub merchant: Option<String>,
    pub category: Option<String>,
    pub group_id: Option<Uuid>,
    pub group_value: Option<String>,
    pub event_type_id: Option<Uuid>,
    pub event_type_value: Option<String>,
    pub start_at: Option<DateTime<Utc>>,
    pub end_at: Option<DateTime<Utc>>,
    pub record_state: Option<String>,
    pub limit: Option<i64>,
    pub cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelinePage {
    pub events: Vec<TimelineEventWithEvidence>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct NewEvidenceItem {
    pub source_record_id: Option<Uuid>,
    pub source_attachment_id: Option<Uuid>,
    pub source_type: String,
    pub source_id: Option<String>,
    pub raw_reference: Option<String>,
    #[serde(default = "empty_metadata")]
    pub observation_metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IngestTimelineEventInput {
    pub event_type_id: Option<Uuid>,
    pub event_type_value: Option<String>,
    pub group_id: Option<Uuid>,
    pub group_value: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub occurred_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default = "default_precision")]
    pub time_precision: String,
    pub source_timezone: Option<String>,
    pub content: serde_json::Value,
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    pub dedupe_key: Option<String>,
    #[serde(default)]
    pub evidence: Vec<NewEvidenceItem>,
}

fn default_precision() -> String {
    "second".to_string()
}

fn default_confidence() -> f64 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TimelineCountsQuery {
    pub start_at: DateTime<Utc>,
    pub end_at: DateTime<Utc>,
    pub timezone: String,
    pub group_value: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct TimelineDayCount {
    pub day: String,
    pub category: String,
    pub count: i64,
}

fn empty_metadata() -> serde_json::Value {
    serde_json::json!({})
}
