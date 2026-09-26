/**
* Domain models for consumer devices and registration credentials.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub user_id: Uuid,
    pub device_identifier: String,
    pub platform: String,
    pub label: String,
    pub public_key: Option<String>,
    pub capabilities: serde_json::Value,
    pub execution_consent: bool,
    pub is_active: bool,
    pub last_seen_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Reads the `local_llm` flag a client reports in its `capabilities` blob at
/// registration time (e.g. the desktop app's Gemma-2B-on-device support).
pub fn local_llm_capable(capabilities: &serde_json::Value) -> bool {
    capabilities
        .get("local_llm")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}
