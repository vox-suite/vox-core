/**
* Domain models for durable tasks, states, and scheduling parameters.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    #[default]
    Pending,
    Evaluating,
    Executing,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    Autonomous,
    Interactive,
    #[default]
    ManualHuman,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: Uuid,
    pub user_id: Uuid,
    pub collection_id: Option<Uuid>,
    pub title: String,
    pub instruction: String,
    pub status: TaskStatus,
    pub priority: i32,
    pub execution_type: ExecutionType,
    pub feasibility_reasoning: Option<String>,
    pub execution_result: serde_json::Value,
    pub due_at: Option<DateTime<Utc>>,
    pub version: i32,
    pub cancellation_requested_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
