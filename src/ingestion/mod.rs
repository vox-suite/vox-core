/**
* Data ingestion pipeline for SMS, emails, and external webhooks.
*/
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEventBatchRequest {
    pub events: Vec<IngestEventItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEventItem {
    pub source_kind: String,
    pub source_id: String,
    pub external_event_id: String,
    pub event_type: String,
    pub payload_version: Option<i32>,
    pub occurred_at: Option<chrono::DateTime<chrono::Utc>>,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestEventBatchResponse {
    pub accepted_count: usize,
    pub deduplicated_count: usize,
    pub event_ids: Vec<Uuid>,
}
