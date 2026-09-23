/**
* Domain models for JSON schema specifications and validation rules.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum SchemaState {
    #[default]
    Active,
    Deprecated,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DataSchema {
    pub id: Uuid,
    pub user_id: Option<Uuid>,
    pub owner_scope: String,
    pub namespace: String,
    pub name: String,
    pub version: i32,
    pub description: String,
    pub json_schema: serde_json::Value,
    pub state: SchemaState,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
