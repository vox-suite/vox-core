/**
* Human-in-the-loop approval workflows for high-privilege agent operations.
*/
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
    pub task_id: Uuid,
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
        };
        if !self
            .grants
            .effective_for_agent(context, &agent)
            .await
            .map_err(|_| ApprovalError::NotFound)?
            .iter()
            .any(|g| g.capability_external_key == capability)
        {
            return Err(ApprovalError::NotFound);
        };
        let mut tx = self.db.pool().begin().await?;
        let task_belongs_to_context = sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM tasks t JOIN task_runs r ON r.task_id=t.id WHERE t.id=$1 AND r.id=$2 AND t.user_context_id=$3 AND r.state IN ('queued','running','waiting'))").bind(r.task_id).bind(r.task_run_id).bind(context.id.0).fetch_one(&mut *tx).await?;
        if !task_belongs_to_context {
            return Err(ApprovalError::NotFound);
        }
        let agent_id=sqlx::query_scalar::<_,Uuid>("SELECT a.id FROM agent_definitions a JOIN deployment_agent_selections s ON s.agent_definition_id=a.id WHERE a.deployment_id=$1 AND a.external_key=$2 AND a.state='enabled'").bind(context.subject.deployment_id.0).bind(agent).fetch_optional(&mut *tx).await?.ok_or(ApprovalError::NotFound)?;
        if let Some(previous) = r.replaces_proposal_id {
            let changed=sqlx::query("UPDATE action_proposals SET state='superseded',updated_at=$3 WHERE id=$1 AND user_context_id=$2 AND state IN ('pending','approved')").bind(previous).bind(context.id.0).bind(now).execute(&mut *tx).await?.rows_affected();
            if changed != 1 {
                return Err(ApprovalError::NotApprovable);
            }
        }
        let hash = hash(&r.details)?;
        let details = r.details;
        let id=sqlx::query_scalar::<_,Uuid>("INSERT INTO action_proposals (user_context_id,task_id,task_run_id,agent_definition_id,capability_external_key,details,details_hash,expires_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id").bind(context.id.0).bind(r.task_id).bind(r.task_run_id).bind(agent_id).bind(&capability).bind(&details).bind(hash).bind(r.expires_at).fetch_one(&mut *tx).await?;
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
        let row=sqlx::query("SELECT details,details_hash,expires_at,state,capability_external_key FROM action_proposals WHERE id=$1 AND user_context_id=$2 FOR UPDATE").bind(proposal_id).bind(context.id.0).fetch_optional(&mut *tx).await?.ok_or(ApprovalError::NotFound)?;
        if !details.is_object() || hash(&details)? != row.get::<Vec<u8>, _>("details_hash") {
            return Err(ApprovalError::NotApprovable);
        }
        let expires: DateTime<Utc> = row.get("expires_at");
        let state: String = row.get("state");
        if expires <= now {
            sqlx::query("UPDATE action_proposals SET state='expired',updated_at=$2 WHERE id=$1 AND state='pending'").bind(proposal_id).bind(now).execute(&mut *tx).await?;
            return Err(ApprovalError::Expired);
        }
        if state != "pending" {
            return Err(ApprovalError::NotApprovable);
        };
        let id=sqlx::query_scalar::<_,Uuid>("INSERT INTO action_approvals (proposal_id,user_context_id,host_app_id,proposal_hash,approved_at) VALUES ($1,$2,$3,$4,$5) RETURNING id").bind(proposal_id).bind(context.id.0).bind(context.subject.host_app_id.0).bind(row.get::<Vec<u8>,_>("details_hash")).bind(now).fetch_one(&mut *tx).await?;
        sqlx::query("UPDATE action_proposals SET state='approved',updated_at=$2 WHERE id=$1")
            .bind(proposal_id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Proposal {
            id: proposal_id,
            capability_external_key: row.get("capability_external_key"),
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
        let row=sqlx::query("SELECT a.proposal_id,a.consumed_attempt_id,a.proposal_hash,p.details,p.details_hash,p.expires_at,p.state FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id WHERE a.id=$1 AND a.user_context_id=$2 FOR UPDATE OF a,p").bind(approval_id).bind(context.id.0).fetch_optional(&mut *tx).await?.ok_or(ApprovalError::NotFound)?;
        let details: Value = row.get("details");
        if hash(&details)? != row.get::<Vec<u8>, _>("proposal_hash")
            || row.get::<Vec<u8>, _>("proposal_hash") != row.get::<Vec<u8>, _>("details_hash")
        {
            return Err(ApprovalError::NotApprovable);
        }
        if row.get::<Option<Uuid>, _>("consumed_attempt_id").is_some() {
            return Err(ApprovalError::Consumed);
        };
        if row.get::<DateTime<Utc>, _>("expires_at") <= now {
            return Err(ApprovalError::Expired);
        };
        if row.get::<String, _>("state") != "approved" {
            return Err(ApprovalError::NotApprovable);
        };
        let proposal_id: Uuid = row.get("proposal_id");
        sqlx::query(
            "UPDATE action_approvals SET consumed_attempt_id=$2,consumed_at=$3 WHERE id=$1",
        )
        .bind(approval_id)
        .bind(attempt_id)
        .bind(now)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE action_proposals SET state='consumed',updated_at=$2 WHERE id=$1")
            .bind(proposal_id)
            .bind(now)
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

fn hash(v: &Value) -> Result<Vec<u8>, ApprovalError> {
    serde_json::to_vec(v)
        .map(|b| Sha256::digest(b).to_vec())
        .map_err(|_| ApprovalError::Invalid)
}
