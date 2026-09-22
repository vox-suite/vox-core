/**
* Event pub/sub bus, domain event definitions, and dispatcher.
*/
pub mod handler;
pub mod service;

use crate::identity::ChannelIdentity;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EventId(pub Uuid);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct IngestEventRequest {
    pub idempotency_key: String,
    pub identity: ChannelIdentity,
    pub event_type: String,
    pub occurred_at: DateTime<Utc>,
    pub payload: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct IngestEventResponse {
    pub event_id: EventId,
}
