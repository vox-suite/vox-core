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
            grants: CapabilityGrantService::new(db.pool().clone()),
            db,
        }
    }

    pub async fn propose(
        &self,
        context: &ResolvedUserContext,
        r: CreateProposalRequest,
        now: DateTime<Utc>,
    ) -> Result<Proposal, ApprovalError> {
        self.propose_inner(context, r, now, None, None).await
    }

    /// A worker can prepare a decision only while it owns the current run
    /// lease. Proposal creation and the approval wait are one transaction.
    pub async fn propose_for_run(
        &self,
        context: &ResolvedUserContext,
        request: CreateProposalRequest,
        now: DateTime<Utc>,
        lease_owner: &str,
        lease_generation: i64,
    ) -> Result<Proposal, ApprovalError> {
        self.propose_inner(
            context,
            request,
            now,
            None,
            Some((lease_owner, lease_generation)),
        )
        .await
    }

    /// Create a proposal and its waiting task atomically. No worker can claim
    /// an unapproved change between task creation and the approval checkpoint.
    pub async fn propose_with_new_task(
        &self,
        context: &ResolvedUserContext,
        r: CreateProposalRequest,
        title: String,
        now: DateTime<Utc>,
    ) -> Result<Proposal, ApprovalError> {
        key(&title, 1024)?;
        crate::agent_registry::AgentRegistry::new(self.db.clone())
            .selected_for_context(context, &r.agent_external_key)
            .await
            .map_err(|_| ApprovalError::Invalid)?;
        self.propose_inner(context, r, now, Some(title), None).await
    }

    async fn propose_inner(
        &self,
        context: &ResolvedUserContext,
        r: CreateProposalRequest,
        now: DateTime<Utc>,
        new_task_title: Option<String>,
        run_fence: Option<(&str, i64)>,
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
            .effective_for_agent(&context.request_context(), &agent)
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
        if let Some((owner, generation)) = run_fence {
            let owned_span=sqlx::query_scalar::<_,Uuid>("SELECT id FROM spans WHERE id=$1 AND user_id=$2 AND user_context_id=$3 AND status<>'cancelled' FOR UPDATE")
                .bind(r.span_id).bind(context.user_id.0).bind(context.id.0).fetch_optional(&mut *tx).await?;
            if owned_span.is_none() {
                return Err(ApprovalError::NotApprovable);
            }
            let valid = sqlx::query_scalar::<_, Uuid>(
                "SELECT j.id FROM jobs j JOIN spans s ON s.id=j.span_id \
                 JOIN assigned_task_runs run ON run.job_id=j.id \
                 WHERE j.id=$1 AND s.id=$2 AND j.user_context_id=$3 \
                 AND s.user_id=$4 AND s.user_context_id=$3 AND run.user_context_id=$3 \
                 AND run.actor_snapshot->'definition'->>'external_key'=$5 \
                 AND j.state='running' AND j.wait_reason IS NULL \
                 AND j.lease_owner=$6 AND j.lease_generation=$7 AND j.lease_expires_at>now() \
                 AND s.status <> 'cancelled' AND run.deadline_at>now() AND j.attempt_count<=j.max_attempts FOR UPDATE OF j,run",
            )
            .bind(r.task_run_id)
            .bind(r.span_id)
            .bind(context.id.0)
            .bind(context.user_id.0)
            .bind(&agent)
            .bind(owner)
            .bind(generation)
            .fetch_optional(&mut *tx)
            .await?;
            if valid.is_none() {
                return Err(ApprovalError::NotApprovable);
            }
        }
        if let Some(title) = new_task_title {
            sqlx::query("INSERT INTO spans(id,user_id,user_context_id,title,notes,status,execution_type) VALUES($1,$2,$3,$4,'Exact connection proposal','planned','interactive')")
                .bind(r.span_id).bind(context.user_id.0).bind(context.id.0).bind(title).execute(&mut *tx).await?;
            sqlx::query("INSERT INTO jobs(id,user_id,user_context_id,kind,payload_reference_id,span_id,state,wait_reason,checkpoint) VALUES($1,$2,$3,'execute_span',$4,$4,'pending','approval',$5)")
                .bind(r.task_run_id).bind(context.user_id.0).bind(context.id.0).bind(r.span_id).bind(serde_json::json!({"agent_external_key":r.agent_external_key})).execute(&mut *tx).await?;
        }
        let task_belongs_to_context = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM spans s JOIN jobs j ON j.span_id=s.id WHERE s.id=$1 AND s.user_id=$2 AND s.user_context_id=$3 AND j.id=$4 AND j.user_context_id=$3)",
        )
        .bind(r.span_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .bind(r.task_run_id)
        .fetch_one(&mut *tx)
        .await?;
        if !task_belongs_to_context {
            return Err(ApprovalError::NotFound);
        }
        if let Some(previous) = r.replaces_proposal_id {
            let changed = sqlx::query(
                "UPDATE action_proposals SET state = 'rejected', updated_at = $3 \
                 WHERE id = $1 AND user_id = $2 AND user_context_id = $4 AND state IN ('proposed', 'approved')",
            )
            .bind(previous)
            .bind(context.user_id.0)
            .bind(now)
            .bind(context.id.0)
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
            "INSERT INTO action_proposals (user_id, user_context_id, span_id, job_id, actor_key, connection_id, capability, details, details_hash, expires_at, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'proposed') RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(context.id.0)
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
        if run_fence.is_some() {
            let waiting = serde_json::json!({"state":"waiting","reason":"approval","checkpoint":{"proposal_id":id,"executed":false}});
            sqlx::query(
                "UPDATE assigned_task_runs SET pending_proposal_id=$2,result=$3 WHERE job_id=$1",
            )
            .bind(r.task_run_id)
            .bind(id)
            .bind(&waiting)
            .execute(&mut *tx)
            .await?;
            sqlx::query("UPDATE jobs SET state='pending',wait_reason='approval',lease_owner=NULL,lease_expires_at=NULL WHERE id=$1")
                .bind(r.task_run_id).execute(&mut *tx).await?;
            sqlx::query("UPDATE spans SET status='waiting_user',execution_result=$2,updated_at=now() WHERE id=$1")
                .bind(r.span_id)
                .bind(waiting)
                .execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(Proposal {
            id,
            capability_external_key: capability,
            expires_at: r.expires_at,
            approval_id: None,
            details,
        })
    }

    pub async fn list_pending(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Proposal>, ApprovalError> {
        let rows=sqlx::query("SELECT p.id,p.capability,p.expires_at,p.details,a.id AS approval_id FROM action_proposals p LEFT JOIN action_approvals a ON a.proposal_id=p.id AND a.user_context_id=p.user_context_id WHERE p.user_context_id=$1 AND p.user_id=$2 AND p.state IN ('proposed','approved') AND (a.id IS NULL OR a.consumed_execution_id IS NULL) AND p.expires_at>now() ORDER BY p.created_at DESC LIMIT 100")
            .bind(context.id.0).bind(context.user_id.0).fetch_all(self.db.pool()).await?;
        Ok(rows
            .into_iter()
            .map(|row| Proposal {
                id: row.get("id"),
                capability_external_key: row.get("capability"),
                expires_at: row.get("expires_at"),
                details: row.get("details"),
                approval_id: row.get("approval_id"),
            })
            .collect())
    }

    pub async fn reject(
        &self,
        context: &ResolvedUserContext,
        proposal_id: Uuid,
    ) -> Result<(), ApprovalError> {
        let changed=sqlx::query("UPDATE action_proposals SET state='rejected',updated_at=now() WHERE id=$1 AND user_id=$2 AND user_context_id=$3 AND state='proposed'")
            .bind(proposal_id).bind(context.user_id.0).bind(context.id.0).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(ApprovalError::NotApprovable);
        }
        Ok(())
    }

    pub async fn approve(
        &self,
        context: &ResolvedUserContext,
        proposal_id: Uuid,
        details: Value,
        now: DateTime<Utc>,
    ) -> Result<Proposal, ApprovalError> {
        let mut tx = self.db.pool().begin().await?;
        // Cancellation and approval serialize on the owned task before either
        // transaction locks its proposal rows.
        let task = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id FROM spans s JOIN action_proposals p ON p.span_id=s.id
             WHERE p.id=$1 AND p.user_id=$2 AND p.user_context_id=$3
               AND s.user_id=$2 AND s.user_context_id=$3 AND s.status<>'cancelled'
             FOR UPDATE OF s",
        )
        .bind(proposal_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if task.is_none() {
            return Err(ApprovalError::NotApprovable);
        }
        let row = sqlx::query(
            "SELECT details, details_hash, expires_at, state, capability \
             FROM action_proposals WHERE id = $1 AND user_id = $2 AND user_context_id = $3 FOR UPDATE",
        )
        .bind(proposal_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
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
            tx.commit().await?;
            return Err(ApprovalError::Expired);
        }
        if state != "proposed" {
            return Err(ApprovalError::NotApprovable);
        }
        let approved_hash: String = row.get("details_hash");
        let session_evidence = serde_json::json!({
            "authority": "authenticated_host_context",
            "user_context_id": context.id.0,
            "host_app_id": context.subject.host_app_id.0,
            "host_user_id": context.subject.host_user_id,
            "organization_id": context.subject.organization_id.map(|id| id.0),
        });
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO action_approvals (proposal_id, user_id, user_context_id, approved_details_hash, session_evidence, approved_at) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(proposal_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
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
             WHERE a.id = $1 AND a.user_id = $2 AND a.user_context_id = $3 AND p.user_context_id = $3 FOR UPDATE OF a, p",
        )
        .bind(approval_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
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
