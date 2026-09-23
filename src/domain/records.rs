/**
* Domain models for dynamic JSONB records and search filters.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum RecordKind {
    #[default]
    Fact,
    Goal,
    Insight,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: Uuid,
    pub user_id: Uuid,
    pub schema_id: Uuid,
    pub schema_scope: String,
    pub kind: RecordKind,
    pub domain: String,
    pub entity_type: String,
    pub title: String,
    pub data: serde_json::Value,
    pub occurred_at: DateTime<Utc>,
    pub source: String,
    pub source_event_id: Option<Uuid>,
    pub source_record_ids: Vec<Uuid>,
    pub collection_id: Option<Uuid>,
    pub valid_until: Option<DateTime<Utc>>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
