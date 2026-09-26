/**
* Storage repository for recording and auditing executed agent actions.
*/
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::actions::{ActionProposal, ActionProposalState};

#[derive(Clone)]
pub struct ActionRepository {
    pool: PgPool,
}

impl ActionRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_proposal(
        &self,
        user_id: Uuid,
        actor_key: &str,
        capability: &str,
        details: serde_json::Value,
        details_hash: &str,
    ) -> Result<ActionProposal, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO action_proposals (user_id, actor_key, capability, details, details_hash)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id, user_id, span_id, job_id, actor_key, connection_id, capability,
                      details, details_hash, state, expires_at, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(actor_key)
        .bind(capability)
        .bind(details)
        .bind(details_hash)
        .fetch_one(&self.pool)
        .await?;

        let state_str: String = row.get("state");

        Ok(ActionProposal {
            id: row.get("id"),
            user_id: row.get("user_id"),
            span_id: row.get("span_id"),
            job_id: row.get("job_id"),
            actor_key: row.get("actor_key"),
            connection_id: row.get("connection_id"),
            capability: row.get("capability"),
            details: row.get("details"),
            details_hash: row.get("details_hash"),
            state: match state_str.as_str() {
                "approved" => ActionProposalState::Approved,
                "rejected" => ActionProposalState::Rejected,
                "expired" => ActionProposalState::Expired,
                _ => ActionProposalState::Proposed,
            },
            expires_at: row.get("expires_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }
}
