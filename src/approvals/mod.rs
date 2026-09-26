use crate::{capability_grants::CapabilityGrantService, db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

const MAX_PROPOSAL_LIFETIME: Duration = Duration::hours(24);

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CreateProposalRequest {
    pub span_id: Uuid,
    pub task_run_id: Uuid,
    pub agent_external_key: String,
    pub capability_external_key: String,
    pub details: Value,
    pub expires_at: DateTime<Utc>,
    pub replaces_proposal_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Proposal {
    pub id: Uuid,
    pub capability_external_key: String,
    pub expires_at: DateTime<Utc>,
    pub approval_id: Option<Uuid>,
    pub details: Value,
}

#[derive(Clone)]
pub struct ApprovalService {
    db: Db,
    grants: CapabilityGrantService,
}

#[derive(Debug, thiserror::Error)]
pub enum ApprovalError {
    #[error("proposal request invalid")]
    Invalid,
    #[error("proposal is unavailable")]
    NotFound,
    #[error("proposal has expired")]
    Expired,
    #[error("proposal cannot be approved")]
    NotApprovable,
    #[error("agent lacks a grant for the proposed connection and capability")]
    UnauthorizedCapability,
    #[error("approval was already consumed")]
    Consumed,
    #[error("approval storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl ApprovalService {
    pub fn new(db: Db) -> Self {
        Self {
            grants: CapabilityGrantService::new(db.clone()),
            db,
        }
    }

    pub async fn propose(
        &self,
        context: &ResolvedUserContext,
        r: CreateProposalRequest,
        now: DateTime<Utc>,
    ) -> Result<Proposal, ApprovalError> {
        let agent = key(&r.agent_external_key, 255)?;
        let capability = key(&r.capability_external_key, 511)?;
        if !r.details.is_object()
            || r.expires_at <= now
            || r.expires_at > now + MAX_PROPOSAL_LIFETIME
        {
            return Err(ApprovalError::Invalid);
        }
        let connection_id = r
            .details
            .get("execution")
            .and_then(|v| v.get("connection_id"))
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok())
            .ok_or(ApprovalError::Invalid)?;
        let grants = self
            .grants
            .effective_for_agent(context, &agent)
            .await
            .map_err(|e| match e {
                crate::capability_grants::CapabilityGrantError::Database(err) => {
                    ApprovalError::Database(err)
                }
                _ => ApprovalError::UnauthorizedCapability,
            })?;
        if !grants.iter().any(|grant| {
            grant.connection_id == connection_id && grant.capability_external_key == capability
        }) {
            return Err(ApprovalError::UnauthorizedCapability);
        }
        let mut tx = self.db.pool().begin().await?;
        let task_belongs_to_context = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM spans WHERE id = $1 AND user_id = $2)",
        )
        .bind(r.span_id)
        .bind(context.user_id.0)
        .fetch_one(&mut *tx)
        .await?;
        if !task_belongs_to_context {
            return Err(ApprovalError::NotFound);
        }
        if let Some(previous) = r.replaces_proposal_id {
            let changed = sqlx::query(
                "UPDATE action_proposals SET state = 'rejected', updated_at = $3 \
                 WHERE id = $1 AND user_id = $2 AND state IN ('proposed', 'approved')",
            )
            .bind(previous)
            .bind(context.user_id.0)
            .bind(now)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed != 1 {
                return Err(ApprovalError::NotApprovable);
            }
        }
        let details_hash = hash(&r.details)?;
        let details = r.details;
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO action_proposals (user_id, span_id, job_id, actor_key, connection_id, capability, details, details_hash, expires_at, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'proposed') RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(r.span_id)
        .bind(r.task_run_id)
        .bind(&agent)
        .bind(Some(connection_id))
        .bind(&capability)
        .bind(&details)
        .bind(&details_hash)
        .bind(r.expires_at)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Proposal {
            id,
            capability_external_key: capability,
            expires_at: r.expires_at,
            approval_id: None,
            details,
        })
    }

    pub async fn approve(
        &self,
        context: &ResolvedUserContext,
        proposal_id: Uuid,
        details: Value,
        now: DateTime<Utc>,
    ) -> Result<Proposal, ApprovalError> {
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "SELECT details, details_hash, expires_at, state, capability \
             FROM action_proposals WHERE id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(proposal_id)
        .bind(context.user_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApprovalError::NotFound)?;
        if !details.is_object() || hash(&details)? != row.get::<String, _>("details_hash") {
            return Err(ApprovalError::NotApprovable);
        }
        let expires: DateTime<Utc> = row.get("expires_at");
        let state: String = row.get("state");
        if expires <= now {
            sqlx::query(
                "UPDATE action_proposals SET state = 'expired', updated_at = $2 \
                 WHERE id = $1 AND state = 'proposed'",
            )
            .bind(proposal_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            return Err(ApprovalError::Expired);
        }
        if state != "proposed" {
            return Err(ApprovalError::NotApprovable);
        }
        let approved_hash: String = row.get("details_hash");
        let session_evidence = serde_json::json!({});
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO action_approvals (proposal_id, user_id, approved_details_hash, session_evidence, approved_at) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(proposal_id)
        .bind(context.user_id.0)
        .bind(&approved_hash)
        .bind(session_evidence)
        .bind(now)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE action_proposals SET state = 'approved', updated_at = $2 WHERE id = $1",
        )
        .bind(proposal_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(Proposal {
            id: proposal_id,
            capability_external_key: row.get("capability"),
            expires_at: expires,
            approval_id: Some(id),
            details: row.get("details"),
        })
    }

    pub async fn consume(
        &self,
        context: &ResolvedUserContext,
        approval_id: Uuid,
        attempt_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), ApprovalError> {
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "SELECT a.proposal_id, a.consumed_execution_id, a.approved_details_hash, \
                    p.details, p.details_hash, p.expires_at, p.state \
             FROM action_approvals a JOIN action_proposals p ON p.id = a.proposal_id \
             WHERE a.id = $1 AND a.user_id = $2 FOR UPDATE OF a, p",
        )
        .bind(approval_id)
        .bind(context.user_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ApprovalError::NotFound)?;
        let details: Value = row.get("details");
        if hash(&details)? != row.get::<String, _>("approved_details_hash")
            || row.get::<String, _>("approved_details_hash") != row.get::<String, _>("details_hash")
        {
            return Err(ApprovalError::NotApprovable);
        }
        if row
            .get::<Option<Uuid>, _>("consumed_execution_id")
            .is_some()
        {
            return Err(ApprovalError::Consumed);
        }
        if row.get::<DateTime<Utc>, _>("expires_at") <= now {
            return Err(ApprovalError::Expired);
        }
        if row.get::<String, _>("state") != "approved" {
            return Err(ApprovalError::NotApprovable);
        }
        sqlx::query("UPDATE action_approvals SET consumed_execution_id = $2 WHERE id = $1")
            .bind(approval_id)
            .bind(attempt_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn key(v: &str, max: usize) -> Result<String, ApprovalError> {
    let v = v.trim();
    if v.is_empty() || v.len() > max {
        Err(ApprovalError::Invalid)
    } else {
        Ok(v.into())
    }
}

fn hash(v: &Value) -> Result<String, ApprovalError> {
    serde_json::to_vec(v)
        .map(|b| hex::encode(Sha256::digest(b)))
        .map_err(|_| ApprovalError::Invalid)
}
