pub use crate::execution_policy::ExecutionIdentity;
use crate::{
    db::Db,
    execution_policy::{ExecutionPolicyService, ExecutionRequest},
    identity::ResolvedUserContext,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize)]
pub struct StartExecutionRequest {
    pub approval_id: Uuid,
    pub idempotency_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Execution {
    pub id: Uuid,
    pub state: String,
    pub provider_reference: Option<String>,
    pub confirmation_evidence: Option<Value>,
}

#[derive(Clone, Debug)]
pub struct AdapterRequest {
    pub execution_id: Uuid,
    pub idempotency_key: String,
    pub identity: ExecutionIdentity,
    pub capability_external_key: String,
}

#[derive(Clone, Debug)]
pub enum AdapterOutcome {
    Succeeded {
        provider_reference: String,
        evidence: Value,
    },
    Failed {
        code: String,
    },
    Cancelled {
        provider_reference: Option<String>,
        evidence: Value,
    },
    AwaitingProviderAuthentication {
        provider_reference: Option<String>,
    },
    Reconciling {
        provider_reference: Option<String>,
    },
    Unknown {
        provider_reference: Option<String>,
        code: String,
    },
}

#[async_trait::async_trait]
pub trait ExecutionAdapter: Send + Sync {
    async fn dispatch(&self, request: AdapterRequest) -> AdapterOutcome;
    async fn reconcile(
        &self,
        request: AdapterRequest,
        provider_reference: Option<&str>,
    ) -> AdapterOutcome;
}

#[derive(Clone)]
pub struct ExecutionCoordinator {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error("execution request invalid")]
    Invalid,
    #[error("execution is unavailable")]
    Unavailable,
    #[error("execution requires a fresh approval")]
    FreshApproval,
    #[error("execution storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl ExecutionCoordinator {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Lock consent before the final task/proposal fence. Revocation takes the
    /// same permission row exclusively, so it cannot race a dispatch claim.
    async fn lock_delegation_authority(
        &self,
        context: &ResolvedUserContext,
        assignment: Option<Uuid>,
        tx: &mut Transaction<'_, Postgres>,
    ) -> Result<(), ExecutionError> {
        if let Some(job) = assignment {
            let denied:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM assigned_task_runs c JOIN assigned_task_runs p ON p.job_id=c.parent_run_id JOIN jobs pj ON pj.id=p.job_id WHERE c.job_id=$1 AND (p.deadline_at<=now() OR p.tool_calls>p.max_tool_calls OR pj.attempt_count>pj.max_attempts OR pj.state IN ('cancelled','failed','completed'))) ").bind(job).fetch_one(&mut **tx).await?;
            if denied {
                return Err(ExecutionError::Unavailable);
            }
            let permission: Option<Uuid> = sqlx::query_scalar(
                "SELECT delegation_permission_id FROM assigned_task_runs WHERE job_id=$1",
            )
            .bind(job)
            .fetch_optional(&mut **tx)
            .await?
            .flatten();
            if let Some(id) = permission {
                let state:Option<String>=sqlx::query_scalar("SELECT state FROM agent_delegation_permissions WHERE id=$1 AND user_context_id=$2 FOR SHARE").bind(id).bind(context.id.0).fetch_optional(&mut **tx).await?;
                if state.as_deref() != Some("enabled") {
                    return Err(ExecutionError::Unavailable);
                }
            }
            crate::delegation::DelegationService::new(self.db.clone())
                .lock_assignment_authority(context, job, tx)
                .await
                .map_err(|_| ExecutionError::Unavailable)?;
        }
        Ok(())
    }

    pub async fn start(
        &self,
        context: &ResolvedUserContext,
        request: StartExecutionRequest,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        let key = request.idempotency_key.trim();
        if key.is_empty() || key.len() > 255 {
            return Err(ExecutionError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;

        if let Some(row) = sqlx::query(
            "SELECT id, approval_id, state, provider_reference, confirmation_evidence FROM executions \
             WHERE user_id = $1 AND user_context_id = $3 AND idempotency_key = $2 FOR UPDATE",
        )
        .bind(context.user_id.0)
        .bind(key)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        {
            if row.get::<Uuid, _>("approval_id") != request.approval_id {
                return Err(ExecutionError::FreshApproval);
            }
            let result = row_execution(row)?;
            tx.commit().await?;
            return Ok(result);
        }
        let assignment:Option<Uuid>=sqlx::query_scalar("SELECT p.job_id FROM action_approvals a JOIN action_proposals p ON p.id=a.proposal_id WHERE a.id=$1 AND a.user_context_id=$2").bind(request.approval_id).bind(context.id.0).fetch_optional(&mut *tx).await?.flatten();
        self.lock_delegation_authority(context, assignment, &mut tx)
            .await?;
        let task = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id FROM spans s JOIN action_proposals p ON p.span_id=s.id
             JOIN action_approvals a ON a.proposal_id=p.id
             WHERE a.id=$1 AND a.user_id=$2 AND a.user_context_id=$3
               AND p.user_context_id=$3 AND s.user_id=$2 AND s.user_context_id=$3
               AND s.status<>'cancelled'
               AND NOT EXISTS (
                   SELECT 1 FROM assigned_task_runs r JOIN jobs j ON j.id=r.job_id
                   WHERE r.job_id=p.job_id AND (r.user_context_id<>$3
                     OR r.deadline_at<=now() OR j.state='cancelled' OR j.wait_reason='budget'
                     OR j.attempt_count>j.max_attempts OR r.tool_calls>r.max_tool_calls)
               ) FOR UPDATE OF s",
        )
        .bind(request.approval_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if task.is_none() {
            return Err(ExecutionError::Unavailable);
        }
        let row = sqlx::query(
            "SELECT a.proposal_id, a.consumed_execution_id, p.details, p.details_hash, p.expires_at, \
                    p.state, p.capability, p.connection_id, p.actor_key \
             FROM action_approvals a JOIN action_proposals p ON p.id = a.proposal_id \
             WHERE a.id = $1 AND a.user_id = $2 AND a.user_context_id = $3 AND p.user_context_id = $3 FOR UPDATE OF a, p",
        )
        .bind(request.approval_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ExecutionError::Unavailable)?;
        if row
            .get::<Option<Uuid>, _>("consumed_execution_id")
            .is_some()
            || row.get::<String, _>("state") != "approved"
            || row.get::<DateTime<Utc>, _>("expires_at") <= now
        {
            return Err(ExecutionError::FreshApproval);
        }
        let details: Value = row.get("details");
        let identity: ExecutionIdentity = serde_json::from_value(
            details
                .get("execution")
                .cloned()
                .ok_or(ExecutionError::FreshApproval)?,
        )
        .map_err(|_| ExecutionError::FreshApproval)?;
        let capability: String = row.get("capability");
        let connection_id = row
            .get::<Option<Uuid>, _>("connection_id")
            .unwrap_or(identity.connection_id);
        let id = Uuid::new_v4();
        let proposal_id: Uuid = row.get("proposal_id");
        let provider_snapshot = serde_json::json!({
            "capability": capability,
            "identity": identity,
        });
        let connection_current = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM retired_external_connections WHERE id=$1 AND user_context_id=$2
             AND authorization_state='authorized' AND (expires_at IS NULL OR expires_at>$3)
             AND $4=ANY(authorized_capabilities) FOR SHARE",
        )
        .bind(connection_id)
        .bind(context.id.0)
        .bind(now)
        .bind(&capability)
        .fetch_optional(&mut *tx)
        .await?;
        if connection_current.is_none() {
            return Err(ExecutionError::Unavailable);
        }
        let grant_current = sqlx::query_scalar::<_, Uuid>(
            "SELECT g.id FROM retired_agent_capability_grants g
             JOIN agent_definitions a ON a.id=g.agent_definition_id
             JOIN retired_external_connections x ON x.id=g.connection_id
             WHERE g.user_context_id=$1 AND g.connection_id=$2
               AND g.capability_external_key=$3 AND g.state='enabled'
               AND a.owner_user_context_id=$1 AND a.external_key=$4 AND a.deployment_id=$5 AND a.state='enabled' AND (a.template_id IS NULL OR EXISTS (SELECT 1 FROM agent_definitions template WHERE template.id=a.template_id AND template.state='enabled' AND ($3=ANY(template.requested_capability_categories) OR '*'=ANY(template.requested_capability_categories))))
               AND ($3=ANY(a.requested_capability_categories) OR '*'=ANY(a.requested_capability_categories))
               AND $3=ANY(x.authorized_capabilities)
               AND x.user_context_id=$1 AND x.authorization_state='authorized'
               AND (x.expires_at IS NULL OR x.expires_at>$6)
               AND (EXISTS (SELECT 1 FROM retired_integration_definitions i
                    WHERE i.id=x.integration_id AND i.deployment_id=$5 AND i.state='enabled')
                 OR EXISTS (SELECT 1 FROM retired_remote_extensions e
                    WHERE e.id=x.remote_extension_id AND e.user_context_id=$1
                      AND e.lifecycle_state='active' AND e.consent_status='consented'
                      AND e.conformance_status='passed' AND e.operator_enabled))
             FOR SHARE OF g,a,x",
        )
        .bind(context.id.0)
        .bind(connection_id)
        .bind(&capability)
        .bind(row.get::<String, _>("actor_key"))
        .bind(context.subject.deployment_id.0)
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        if grant_current.is_none() {
            return Err(ExecutionError::Unavailable);
        }
        let decision = ExecutionPolicyService::new(self.db.clone())
            .evaluate_in_transaction(
                context,
                ExecutionRequest {
                    approval_id: request.approval_id,
                    attempt_id: id,
                    execution: identity.clone(),
                },
                now,
                &mut tx,
            )
            .await
            .map_err(|_| ExecutionError::Unavailable)?;
        let request_hash = hex::encode(Sha256::digest(key.as_bytes()));
        sqlx::query(
            "INSERT INTO executions (id, user_id, user_context_id, approval_id, proposal_id, connection_id, idempotency_key, provider_snapshot, policy_snapshot, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending')",
        )
        .bind(id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .bind(request.approval_id)
        .bind(proposal_id)
        .bind(connection_id)
        .bind(key)
        .bind(provider_snapshot)
        .bind(decision.policy_snapshot)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO execution_attempts (execution_id, attempt_number, request_hash, state) \
             VALUES ($1, 1, $2, 'started')",
        )
        .bind(id)
        .bind(request_hash)
        .execute(&mut *tx)
        .await?;
        sqlx::query("UPDATE action_approvals SET consumed_execution_id = $2 WHERE id = $1")
            .bind(request.approval_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Execution {
            id,
            state: "pending".into(),
            provider_reference: None,
            confirmation_evidence: None,
        })
    }

    /// Claim a not-yet-dispatched execution against the current task state.
    /// Once claimed, cancellation cannot assert that an external effect stopped;
    /// its eventual result must still be recorded or reconciled.
    pub async fn claim_dispatch(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<(), ExecutionError> {
        let assignment:Option<Uuid>=sqlx::query_scalar("SELECT p.job_id FROM executions e JOIN action_proposals p ON p.id=e.proposal_id WHERE e.id=$1 AND e.user_context_id=$2").bind(execution_id).bind(context.id.0).fetch_optional(self.db.pool()).await?.flatten();
        let mut tx = self.db.pool().begin().await?;
        self.lock_delegation_authority(context, assignment, &mut tx)
            .await?;

        let task = sqlx::query_scalar::<_, Uuid>(
            "SELECT s.id FROM spans s JOIN action_proposals p ON p.span_id=s.id
             JOIN executions e ON e.proposal_id=p.id
             WHERE e.id=$1 AND e.user_id=$2 AND e.user_context_id=$3
               AND p.user_context_id=$3 AND s.user_id=$2 AND s.user_context_id=$3
               AND s.status<>'cancelled'
               AND NOT EXISTS (
                   SELECT 1 FROM assigned_task_runs r JOIN jobs j ON j.id=r.job_id
                   WHERE r.job_id=p.job_id AND (r.user_context_id<>$3
                     OR r.deadline_at<=now() OR j.state='cancelled' OR j.wait_reason='budget'
                     OR j.attempt_count>j.max_attempts OR r.tool_calls>r.max_tool_calls)
               ) FOR UPDATE OF s",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if task.is_none() {
            return Err(ExecutionError::Unavailable);
        }
        let changed = sqlx::query(
            "UPDATE executions SET state='reconciling',updated_at=$4
             WHERE id=$1 AND user_id=$2 AND user_context_id=$3 AND state='pending'",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(ExecutionError::Unavailable);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn dispatch(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        adapter: &dyn ExecutionAdapter,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        let request = self.adapter_request(context, execution_id).await?;
        self.claim_dispatch(context, execution_id, now).await?;
        let outcome = adapter.dispatch(request).await;
        self.record_dispatched_outcome(context, execution_id, outcome, now)
            .await
    }

    pub async fn reconcile(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        adapter: &dyn ExecutionAdapter,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        let request = self.adapter_request(context, execution_id).await?;
        let row = sqlx::query(
            "SELECT state, provider_reference FROM executions WHERE id = $1 AND user_id = $2 AND user_context_id = $3",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ExecutionError::Unavailable)?;
        let state: String = row.get("state");
        if !matches!(state.as_str(), "reconciling" | "in_progress") {
            return Err(ExecutionError::Unavailable);
        }
        let reference: Option<String> = row.get("provider_reference");
        self.record_reconciled_outcome(
            context,
            execution_id,
            adapter.reconcile(request, reference.as_deref()).await,
            now,
        )
        .await
    }

    async fn adapter_request(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
    ) -> Result<AdapterRequest, ExecutionError> {
        let row = sqlx::query(
            "SELECT idempotency_key, provider_snapshot FROM executions \
             WHERE id = $1 AND user_id = $2 AND user_context_id = $3",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ExecutionError::Unavailable)?;
        let snapshot: Value = row.get("provider_snapshot");
        Ok(AdapterRequest {
            execution_id,
            idempotency_key: row.get::<String, _>("idempotency_key"),
            identity: serde_json::from_value(
                snapshot
                    .get("identity")
                    .cloned()
                    .ok_or(ExecutionError::Invalid)?,
            )
            .map_err(|_| ExecutionError::Invalid)?,
            capability_external_key: snapshot
                .get("capability")
                .and_then(|v| v.as_str())
                .ok_or(ExecutionError::Invalid)?
                .to_owned(),
        })
    }

    pub async fn record_outcome(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        self.persist_outcome(context, execution_id, outcome, now, false)
            .await
    }

    /// Records a result after a remote dispatch has begun. An ambiguous
    /// result remains reconciling and must never trigger an automatic retry.
    pub async fn record_dispatched_outcome(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        self.persist_outcome(context, execution_id, outcome, now, true)
            .await
    }

    pub async fn record_verified_external_outcome(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        self.persist_outcome(context, execution_id, outcome, now, true)
            .await
    }

    pub async fn record_verified_external_outcome_in_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        self.persist_outcome_in_transaction(transaction, context, execution_id, outcome, now, true)
            .await
    }

    pub async fn get(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
    ) -> Result<Execution, ExecutionError> {
        let row = sqlx::query(
            "SELECT id, state, provider_reference, confirmation_evidence FROM executions \
             WHERE id = $1 AND user_id = $2 AND user_context_id = $3",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ExecutionError::Unavailable)?;
        row_execution(row)
    }

    async fn record_reconciled_outcome(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        self.persist_outcome(context, execution_id, outcome, now, true)
            .await
    }

    async fn persist_outcome(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
        allow_reconciling_transition: bool,
    ) -> Result<Execution, ExecutionError> {
        let mut transaction = self.db.pool().begin().await?;
        let execution = self
            .persist_outcome_in_transaction(
                &mut transaction,
                context,
                execution_id,
                outcome,
                now,
                allow_reconciling_transition,
            )
            .await?;
        transaction.commit().await?;
        Ok(execution)
    }

    async fn persist_outcome_in_transaction(
        &self,
        transaction: &mut Transaction<'_, Postgres>,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        outcome: AdapterOutcome,
        now: DateTime<Utc>,
        allow_reconciling_transition: bool,
    ) -> Result<Execution, ExecutionError> {
        let (state, reference, evidence, done) = match outcome {
            AdapterOutcome::Succeeded {
                provider_reference,
                evidence,
            } if confirmation_evidence_is_present(&evidence) => {
                ("succeeded", Some(provider_reference), Some(evidence), true)
            }
            AdapterOutcome::Succeeded { .. } => return Err(ExecutionError::Invalid),
            AdapterOutcome::Failed { .. } => ("failed", None, None, true),
            AdapterOutcome::Cancelled {
                provider_reference,
                evidence,
            } if confirmation_evidence_is_present(&evidence) => {
                ("failed", provider_reference, Some(evidence), true)
            }
            AdapterOutcome::Cancelled { .. } => return Err(ExecutionError::Invalid),
            AdapterOutcome::AwaitingProviderAuthentication { provider_reference } => {
                ("in_progress", provider_reference, None, false)
            }
            AdapterOutcome::Reconciling { provider_reference } => {
                ("reconciling", provider_reference, None, false)
            }
            AdapterOutcome::Unknown {
                provider_reference,
                code: _,
            } if allow_reconciling_transition => ("reconciling", provider_reference, None, false),
            AdapterOutcome::Unknown {
                provider_reference,
                code: _,
            } => ("failed", provider_reference, None, true),
        };
        let row = sqlx::query(
            "UPDATE executions SET state = $1, provider_reference = COALESCE($2, provider_reference), \
             confirmation_evidence = COALESCE($3, confirmation_evidence), updated_at = $4, \
             completed_at = CASE WHEN $5 THEN $4 ELSE NULL END \
             WHERE id = $6 AND user_id = $7 AND user_context_id = $9 AND state NOT IN ('succeeded', 'failed') \
             AND (($8 AND state IN ('reconciling', 'in_progress')) OR (NOT $8 AND state <> 'reconciling')) \
             RETURNING id, state, provider_reference, confirmation_evidence",
        )
        .bind(state)
        .bind(reference)
        .bind(evidence)
        .bind(now)
        .bind(done)
        .bind(execution_id)
        .bind(context.user_id.0)
        .bind(allow_reconciling_transition)
        .bind(context.id.0)
        .fetch_optional(&mut **transaction)
        .await?
        .ok_or(ExecutionError::Unavailable)?;
        row_execution(row)
    }
}

fn row_execution(row: sqlx::postgres::PgRow) -> Result<Execution, ExecutionError> {
    Ok(Execution {
        id: row.try_get("id")?,
        state: row.try_get("state")?,
        provider_reference: row.try_get("provider_reference")?,
        confirmation_evidence: row.try_get("confirmation_evidence")?,
    })
}

fn confirmation_evidence_is_present(evidence: &Value) -> bool {
    evidence
        .as_object()
        .is_some_and(|object| !object.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        approvals::ApprovalService,
        durable_tasks::{DurableTaskService, StartTaskRequest},
        identity::IdentityService,
    };
    use serde_json::json;

    struct MockAdapter(std::sync::atomic::AtomicUsize);
    #[async_trait::async_trait]
    impl ExecutionAdapter for MockAdapter {
        async fn dispatch(&self, _: AdapterRequest) -> AdapterOutcome {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            AdapterOutcome::Failed {
                code: "unexpected_dispatch".into(),
            }
        }
        async fn reconcile(&self, _: AdapterRequest, _: Option<&str>) -> AdapterOutcome {
            AdapterOutcome::Succeeded {
                provider_reference: "fixture-confirmed".into(),
                evidence: json!({"verified": true}),
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires disposable PostgreSQL"]
    async fn cancelled_task_denies_new_approval_start_and_dispatch() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").expect("isolated database"))
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let context = IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        let tasks = DurableTaskService::new(db.clone());
        let approvals = ApprovalService::new(db.clone());
        let executions = ExecutionCoordinator::new(db.clone());
        for scenario in [
            "approve",
            "start",
            "dispatch",
            "reconcile",
            "budget_start",
            "deadline_start",
            "budget_dispatch",
            "deadline_dispatch",
        ] {
            let task = tasks
                .start(
                    &context,
                    StartTaskRequest {
                        title: scenario.into(),
                        instruction: "Explicit assigned work".into(),
                        agent_external_key: None,
                    },
                )
                .await
                .unwrap();
            let details = json!({"fixture":"exact approved content"});
            let digest = hex::encode(Sha256::digest(serde_json::to_vec(&details).unwrap()));
            let proposal:Uuid = sqlx::query_scalar("INSERT INTO action_proposals(user_id,user_context_id,span_id,job_id,actor_key,capability,details,details_hash,expires_at) VALUES($1,$2,$3,$4,$5,'fixture.write',$6,$7,now()+interval '1 hour') RETURNING id")
                .bind(user).bind(context.id.0).bind(task.id).bind(task.run_id).bind(task.agent_external_key.as_ref().unwrap()).bind(&details).bind(digest).fetch_one(db.pool()).await.unwrap();
            let approval = if scenario == "approve" {
                None
            } else {
                Some(
                    approvals
                        .approve(&context, proposal, details.clone(), Utc::now())
                        .await
                        .unwrap()
                        .approval_id
                        .unwrap(),
                )
            };
            let execution = if scenario.ends_with("dispatch") || scenario == "reconcile" {
                // A persisted, consumed approval is still not provider dispatch.
                let id:Uuid = sqlx::query_scalar("INSERT INTO executions(user_id,user_context_id,proposal_id,approval_id,idempotency_key) VALUES($1,$2,$3,$4,$5) RETURNING id")
                    .bind(user).bind(context.id.0).bind(proposal).bind(approval).bind(scenario).fetch_one(db.pool()).await.unwrap();
                let snapshot = json!({"capability":"fixture.write","identity":{"provider_external_key":"fixture","model_identifier":"fixture","account_reference":"fixture","connection_id":Uuid::new_v4(),"price_amount_minor":0,"price_currency":"USD"}});
                sqlx::query("UPDATE executions SET provider_snapshot=$2 WHERE id=$1")
                    .bind(id)
                    .bind(snapshot)
                    .execute(db.pool())
                    .await
                    .unwrap();
                sqlx::query("UPDATE action_approvals SET consumed_execution_id=$2 WHERE id=$1")
                    .bind(approval)
                    .bind(id)
                    .execute(db.pool())
                    .await
                    .unwrap();
                Some(id)
            } else {
                None
            };
            if scenario == "reconcile" {
                executions
                    .claim_dispatch(&context, execution.unwrap(), Utc::now())
                    .await
                    .unwrap();
            }
            if scenario == "reconcile" {
                sqlx::query("UPDATE assigned_task_runs SET pending_proposal_id=$2 WHERE job_id=$1")
                    .bind(task.run_id)
                    .bind(proposal)
                    .execute(db.pool())
                    .await
                    .unwrap();
                sqlx::query("UPDATE jobs SET state='pending',wait_reason='approval' WHERE id=$1")
                    .bind(task.run_id)
                    .execute(db.pool())
                    .await
                    .unwrap();
                let waiting = tasks.get(&context, task.id).await.unwrap();
                assert_eq!(waiting.state, crate::durable_tasks::RunState::Waiting);
                assert_eq!(
                    waiting.wait_reason,
                    Some(crate::durable_tasks::WaitReason::Reconciliation)
                );
                assert_eq!(
                    waiting.result["checkpoint"]["code"],
                    "provider_outcome_unconfirmed"
                );
            }
            if scenario.starts_with("budget_") {
                sqlx::query("UPDATE jobs SET state='pending',wait_reason='budget' WHERE id=$1")
                    .bind(task.run_id)
                    .execute(db.pool())
                    .await
                    .unwrap();
            } else if scenario.starts_with("deadline_") {
                // Construct an already-expired binding as fixture data. The
                // production immutable-update trigger remains enabled.
                sqlx::query("WITH removed AS (DELETE FROM assigned_task_runs WHERE job_id=$1 RETURNING *) INSERT INTO assigned_task_runs SELECT (jsonb_populate_record(NULL::assigned_task_runs,to_jsonb(removed)||jsonb_build_object('deadline_at',now()-interval '1 second'))).* FROM removed")
                    .bind(task.run_id).execute(db.pool()).await.unwrap();
            } else {
                tasks.cancel(&context, task.id).await.unwrap();
            }
            if scenario.ends_with("dispatch") {
                let adapter = MockAdapter(std::sync::atomic::AtomicUsize::new(0));
                assert!(matches!(
                    executions
                        .dispatch(&context, execution.unwrap(), &adapter, Utc::now())
                        .await,
                    Err(ExecutionError::Unavailable)
                ));
                assert_eq!(adapter.0.load(std::sync::atomic::Ordering::SeqCst), 0);
            }
            match scenario {
                "approve" => assert!(matches!(
                    approvals
                        .approve(&context, proposal, details, Utc::now())
                        .await,
                    Err(crate::approvals::ApprovalError::NotApprovable)
                )),
                "start" | "budget_start" | "deadline_start" => assert!(matches!(
                    executions
                        .start(
                            &context,
                            StartExecutionRequest {
                                approval_id: approval.unwrap(),
                                idempotency_key: scenario.into()
                            },
                            Utc::now()
                        )
                        .await,
                    Err(ExecutionError::Unavailable)
                )),
                "dispatch" | "budget_dispatch" | "deadline_dispatch" => assert!(matches!(
                    executions
                        .claim_dispatch(&context, execution.unwrap(), Utc::now())
                        .await,
                    Err(ExecutionError::Unavailable)
                )),
                "reconcile" => {
                    let stopped = tasks.get(&context, task.id).await.unwrap();
                    assert_eq!(stopped.state, crate::durable_tasks::RunState::Cancelled);
                    assert_eq!(
                        stopped.result["checkpoint"]["code"],
                        "provider_outcome_unconfirmed"
                    );
                    assert!(
                        executions
                            .claim_dispatch(&context, execution.unwrap(), Utc::now())
                            .await
                            .is_err()
                    );
                    let reconciled = executions
                        .reconcile(
                            &context,
                            execution.unwrap(),
                            &MockAdapter(std::sync::atomic::AtomicUsize::new(0)),
                            Utc::now(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(reconciled.state, "succeeded");
                    let existing = executions
                        .start(
                            &context,
                            StartExecutionRequest {
                                approval_id: approval.unwrap(),
                                idempotency_key: scenario.into(),
                            },
                            Utc::now(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(existing.id, execution.unwrap());
                }
                _ => unreachable!(),
            }
        }
    }
}
