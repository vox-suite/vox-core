use crate::{
    db::Db,
    identity::{
        DeploymentId, HostAppId, HostOrganizationId, ResolvedUserContext, UserContextId,
        UserContextSubject, UserId,
    },
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Queued,
    Running,
    Waiting,
    Completed,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Clarification,
    Connection,
    Approval,
    Authentication,
    Reconciliation,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StartTaskRequest {
    pub title: String,
    pub instruction: String,
    pub agent_external_key: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct WaitRequest {
    pub reason: WaitReason,
    #[serde(default)]
    pub checkpoint: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct DurableTask {
    pub id: Uuid,
    pub title: String,
    pub state: RunState,
    pub run_id: Uuid,
    pub wait_reason: Option<WaitReason>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ClaimedRun {
    pub task: DurableTask,
    pub checkpoint: Value,
}

#[derive(Clone)]
pub struct DurableTaskService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum DurableTaskError {
    #[error("task request is invalid")]
    Invalid,
    #[error("task is unavailable")]
    NotFound,
    #[error("task cannot transition from its current state")]
    Conflict,
    #[error("task storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl DurableTaskService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn start(
        &self,
        context: &ResolvedUserContext,
        request: StartTaskRequest,
    ) -> Result<DurableTask, DurableTaskError> {
        let title = text(&request.title, 1024)?;
        let instruction = text(&request.instruction, 20_000)?;
        if let Some(agent) = request.agent_external_key.as_deref() {
            text(agent, 255)?;
        }
        let mut tx = self.db.pool().begin().await?;
        let span_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO spans (user_id, title, notes, status, execution_type) VALUES ($1, $2, $3, 'planned', 'interactive') RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(title.clone())
        .bind(instruction)
        .fetch_one(&mut *tx)
        .await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO jobs (user_id, kind, span_id, state, checkpoint) VALUES ($1, 'execute_span', $2, 'pending', '{}'::jsonb) RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(span_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(DurableTask {
            id: span_id,
            title,
            state: RunState::Queued,
            run_id,
            wait_reason: None,
        })
    }

    pub async fn get(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        self.load(context, span_id).await
    }

    pub async fn wait(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
        request: WaitRequest,
    ) -> Result<DurableTask, DurableTaskError> {
        if !request.checkpoint.is_object() {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, span_id).await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state='pending', wait_reason=$2, checkpoint=$3, lease_owner=NULL, lease_expires_at=NULL WHERE span_id=$1 AND kind='execute_span' AND ((state='pending' AND wait_reason IS NULL) OR state='running')",
        )
        .bind(span_id)
        .bind(wait_name(&request.reason))
        .bind(request.checkpoint)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE spans SET status='waiting_user', updated_at=now() WHERE id=$1")
            .bind(span_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.load(context, span_id).await
    }

    pub async fn resume(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, span_id).await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state='pending', wait_reason=NULL WHERE span_id=$1 AND kind='execute_span' AND state='pending' AND wait_reason IS NOT NULL",
        )
        .bind(span_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE spans SET status='planned', updated_at=now() WHERE id=$1")
            .bind(span_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.load(context, span_id).await
    }

    pub async fn cancel(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, span_id).await?;
        let changed = sqlx::query(
            "UPDATE jobs SET state='cancelled', wait_reason=NULL, lease_owner=NULL, lease_expires_at=NULL, completed_at=now() WHERE span_id=$1 AND kind='execute_span' AND state IN ('pending', 'running')",
        )
        .bind(span_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query(
            "UPDATE spans SET status='cancelled', cancellation_requested_at=now(), completed_at=now(), updated_at=now() WHERE id=$1",
        )
        .bind(span_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.load(context, span_id).await
    }

    pub async fn recover_expired(&self, now: DateTime<Utc>) -> Result<u64, DurableTaskError> {
        Ok(sqlx::query(
            "UPDATE jobs SET state='pending', lease_owner=NULL, lease_expires_at=NULL WHERE state='running' AND lease_expires_at <= $1",
        )
        .bind(now)
        .execute(self.db.pool())
        .await?
        .rows_affected())
    }

    pub async fn claim_next(
        &self,
        worker: &str,
        now: DateTime<Utc>,
        lease: Duration,
    ) -> Result<Option<ClaimedRun>, DurableTaskError> {
        if text(worker, 255).is_err() {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "WITH candidate AS (SELECT j.id FROM jobs j JOIN spans t ON t.id=j.span_id WHERE j.kind='execute_span' AND ((j.state='pending' AND j.wait_reason IS NULL) OR (j.state='running' AND j.lease_expires_at <= $1)) AND t.status <> 'cancelled' ORDER BY j.created_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='running', lease_owner=$2, lease_expires_at=$3 FROM candidate WHERE j.id=candidate.id RETURNING j.id, j.span_id, j.checkpoint",
        )
        .bind(now)
        .bind(worker)
        .bind(now + lease)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let run_id: Uuid = row.get("id");
        let span_id: Uuid = row.get("span_id");
        let checkpoint: Value = row.get("checkpoint");
        sqlx::query("UPDATE spans SET status='active', updated_at=now() WHERE id=$1")
            .bind(span_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let context = self.context_for_task(span_id).await?;
        let mut task = self.load(&context, span_id).await?;
        task.run_id = run_id;
        task.state = RunState::Running;
        Ok(Some(ClaimedRun { task, checkpoint }))
    }

    async fn lock_task(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        context: &ResolvedUserContext,
        span_id: Uuid,
    ) -> Result<(), DurableTaskError> {
        let exists = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM spans WHERE id=$1 AND user_id=$2 FOR UPDATE",
        )
        .bind(span_id)
        .bind(context.user_id.0)
        .fetch_optional(&mut **tx)
        .await?;
        if exists.is_some() {
            Ok(())
        } else {
            Err(DurableTaskError::NotFound)
        }
    }

    async fn context_for_task(
        &self,
        span_id: Uuid,
    ) -> Result<ResolvedUserContext, DurableTaskError> {
        let row = sqlx::query(
            "SELECT t.user_id, c.id AS context_id, c.deployment_id, c.host_app_id, \
                    c.organization_id, c.host_user_id \
             FROM spans t JOIN user_contexts c \
               ON c.id = t.user_context_id AND c.user_id = t.user_id \
             WHERE t.id = $1",
        )
        .bind(span_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(DurableTaskError::NotFound)?;
        Ok(ResolvedUserContext {
            id: UserContextId(row.get("context_id")),
            user_id: UserId(row.get("user_id")),
            subject: UserContextSubject {
                deployment_id: DeploymentId(row.get("deployment_id")),
                host_app_id: HostAppId(row.get("host_app_id")),
                organization_id: row
                    .get::<Option<Uuid>, _>("organization_id")
                    .map(HostOrganizationId),
                host_user_id: row.get("host_user_id"),
            },
        })
    }

    async fn load(
        &self,
        context: &ResolvedUserContext,
        span_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let row = sqlx::query(
            "SELECT t.title, j.id, j.state, j.wait_reason FROM spans t JOIN jobs j ON j.span_id=t.id WHERE t.id=$1 AND t.user_id=$2 ORDER BY j.created_at DESC LIMIT 1",
        )
        .bind(span_id)
        .bind(context.user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(DurableTaskError::NotFound)?;
        Ok(DurableTask {
            id: span_id,
            title: row.get(0),
            run_id: row.get(1),
            state: run_state(
                &row.get::<String, _>(2),
                row.get::<Option<String>, _>(3).as_deref(),
            )?,
            wait_reason: row
                .get::<Option<String>, _>(3)
                .map(|v| wait(&v))
                .transpose()?,
        })
    }
}

fn text(v: &str, max: usize) -> Result<String, DurableTaskError> {
    let v = v.trim();
    if v.is_empty() || v.len() > max {
        Err(DurableTaskError::Invalid)
    } else {
        Ok(v.into())
    }
}

fn run_state(job_state: &str, wait_reason: Option<&str>) -> Result<RunState, DurableTaskError> {
    match job_state {
        "running" => Ok(RunState::Running),
        "completed" => Ok(RunState::Completed),
        "cancelled" => Ok(RunState::Cancelled),
        "failed" => Ok(RunState::Failed),
        "pending" => {
            if wait_reason.is_some() {
                Ok(RunState::Waiting)
            } else {
                Ok(RunState::Queued)
            }
        }
        _ => Err(DurableTaskError::Invalid),
    }
}

fn wait(v: &str) -> Result<WaitReason, DurableTaskError> {
    match v {
        "clarification" => Ok(WaitReason::Clarification),
        "connection" => Ok(WaitReason::Connection),
        "approval" => Ok(WaitReason::Approval),
        "authentication" => Ok(WaitReason::Authentication),
        "reconciliation" => Ok(WaitReason::Reconciliation),
        _ => Err(DurableTaskError::Invalid),
    }
}

fn wait_name(v: &WaitReason) -> &'static str {
    match v {
        WaitReason::Clarification => "clarification",
        WaitReason::Connection => "connection",
        WaitReason::Approval => "approval",
        WaitReason::Authentication => "authentication",
        WaitReason::Reconciliation => "reconciliation",
    }
}
