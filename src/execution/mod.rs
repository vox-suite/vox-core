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
            "SELECT id, state, provider_reference, confirmation_evidence FROM executions \
             WHERE user_id = $1 AND idempotency_key = $2 FOR UPDATE",
        )
        .bind(context.user_id.0)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?
        {
            let result = row_execution(row)?;
            tx.commit().await?;
            return Ok(result);
        }
        let row = sqlx::query(
            "SELECT a.proposal_id, a.consumed_execution_id, p.details, p.details_hash, p.expires_at, \
                    p.state, p.capability, p.connection_id, p.actor_key \
             FROM action_approvals a JOIN action_proposals p ON p.id = a.proposal_id \
             WHERE a.id = $1 AND a.user_id = $2 FOR UPDATE OF a, p",
        )
        .bind(request.approval_id)
        .bind(context.user_id.0)
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
            "SELECT id FROM connections WHERE id=$1 AND user_context_id=$2
             AND authorization_state='authorized' AND (expires_at IS NULL OR expires_at>$3)
             AND $4=ANY(allowed_capabilities) FOR SHARE",
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
            "SELECT g.id FROM agent_capability_grants g
             JOIN agent_definitions a ON a.id=g.agent_definition_id
             JOIN external_connections x ON x.id=g.connection_id
             JOIN integration_definitions i ON i.id=x.integration_id
             WHERE g.user_context_id=$1 AND g.connection_id=$2
               AND g.capability_external_key=$3 AND g.state='enabled'
               AND a.external_key=$4 AND a.deployment_id=$5 AND a.state='enabled'
               AND x.user_context_id=$1 AND x.authorization_state='authorized'
               AND (x.expires_at IS NULL OR x.expires_at>$6) AND i.state='enabled'
             FOR SHARE OF g,a,x,i",
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
            "INSERT INTO executions (id, user_id, approval_id, proposal_id, connection_id, idempotency_key, provider_snapshot, policy_snapshot, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')",
        )
        .bind(id)
        .bind(context.user_id.0)
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

    pub async fn dispatch(
        &self,
        context: &ResolvedUserContext,
        execution_id: Uuid,
        adapter: &dyn ExecutionAdapter,
        now: DateTime<Utc>,
    ) -> Result<Execution, ExecutionError> {
        let request = self.adapter_request(context, execution_id).await?;
        self.record_outcome(
            context,
            execution_id,
            AdapterOutcome::Reconciling {
                provider_reference: None,
            },
            now,
        )
        .await?;
        let outcome = adapter.dispatch(request).await;
        self.record_outcome(context, execution_id, outcome, now)
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
            "SELECT state, provider_reference FROM executions WHERE id = $1 AND user_id = $2",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
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
             WHERE id = $1 AND user_id = $2",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
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
             WHERE id = $1 AND user_id = $2",
        )
        .bind(execution_id)
        .bind(context.user_id.0)
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
             WHERE id = $6 AND user_id = $7 AND state NOT IN ('succeeded', 'failed') \
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
