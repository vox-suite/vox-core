/**
 * Domain models for durable tasks, states, and scheduling parameters.
 */

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Pending,
    Evaluating,
    Executing,
    WaitingUser,
    Completed,
    Failed,
    Cancelled,
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Pending
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionType {
    Autonomous,
    Interactive,
    ManualHuman,
}

impl Default for ExecutionType {
    fn default() -> Self {
        ExecutionType::ManualHuman
    }
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
