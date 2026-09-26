/**
* Domain models for agent actions, audit payloads, and execution results.
*/
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionProposalState {
    Proposed,
    Approved,
    Rejected,
    Expired,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionProposal {
    pub id: Uuid,
    pub user_id: Uuid,
    pub span_id: Option<Uuid>,
    pub job_id: Option<Uuid>,
    pub actor_key: String,
    pub connection_id: Option<Uuid>,
    pub capability: String,
    pub details: serde_json::Value,
    pub details_hash: String,
    pub state: ActionProposalState,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActionApproval {
    pub id: Uuid,
    pub proposal_id: Uuid,
    pub user_id: Uuid,
    pub approved_details_hash: String,
    pub session_evidence: serde_json::Value,
    pub approved_at: DateTime<Utc>,
    pub consumed_execution_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Pending,
    InProgress,
    Succeeded,
    Failed,
    Reconciling,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Execution {
    pub id: Uuid,
    pub user_id: Uuid,
    pub proposal_id: Uuid,
    pub approval_id: Uuid,
    pub connection_id: Option<Uuid>,
    pub idempotency_key: String,
    pub provider_snapshot: serde_json::Value,
    pub state: ExecutionState,
    pub provider_reference: Option<String>,
    pub confirmation_evidence: serde_json::Value,
    pub policy_snapshot: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}
