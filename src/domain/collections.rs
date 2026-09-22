/**
 * Domain models for dynamic record collections and schema bindings.
 */

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectionKind {
    Project,
    Trip,
    Course,
    Area,
}

impl Default for CollectionKind {
    fn default() -> Self {
        CollectionKind::Project
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CollectionStatus {
    Active,
    Paused,
    Completed,
    Archived,
}

impl Default for CollectionStatus {
    fn default() -> Self {
        CollectionStatus::Active
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Collection {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub description: String,
    pub kind: CollectionKind,
    pub status: CollectionStatus,
    pub metadata: serde_json::Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
