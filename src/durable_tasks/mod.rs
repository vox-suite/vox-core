/**
 * Durable task state machine, execution tracking, and retries.
 */

use crate::{db::Db, identity::ResolvedUserContext};
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
        let mut tx = self.db.pool().begin().await?;
        if let Some(agent) = request.agent_external_key.as_deref() {
            self.assert_selected(&mut tx, context, agent).await?;
        }
        let task_id = sqlx::query_scalar::<_, Uuid>("INSERT INTO tasks (user_context_id,user_id,title,raw_instruction,status,execution_type) VALUES ($1,$2,$3,$4,'pending','interactive') RETURNING id")
            .bind(context.id.0).bind(context.user_id.0).bind(title.clone()).bind(instruction).fetch_one(&mut *tx).await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO task_runs (task_id) VALUES ($1) RETURNING id",
        )
        .bind(task_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(DurableTask {
            id: task_id,
            title,
            state: RunState::Queued,
            run_id,
            wait_reason: None,
        })
    }

    pub async fn get(
        &self,
        context: &ResolvedUserContext,
        task_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        self.load(context, task_id).await
    }

    pub async fn wait(
        &self,
        context: &ResolvedUserContext,
        task_id: Uuid,
        request: WaitRequest,
    ) -> Result<DurableTask, DurableTaskError> {
        if !request.checkpoint.is_object() {
            return Err(DurableTaskError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, task_id).await?;
        let changed = sqlx::query("UPDATE task_runs SET state='waiting',wait_reason=$2,checkpoint=$3,lease_owner=NULL,lease_expires_at=NULL,updated_at=now() WHERE task_id=$1 AND state IN ('queued','running')")
            .bind(task_id).bind(wait_name(&request.reason)).bind(request.checkpoint).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE tasks SET status='waiting_user',updated_at=now() WHERE id=$1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.load(context, task_id).await
    }

    pub async fn resume(
        &self,
        context: &ResolvedUserContext,
        task_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, task_id).await?;
        let changed = sqlx::query("UPDATE task_runs SET state='queued',wait_reason=NULL,updated_at=now() WHERE task_id=$1 AND state='waiting'").bind(task_id).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE tasks SET status='pending',updated_at=now() WHERE id=$1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.load(context, task_id).await
    }

    pub async fn cancel(
        &self,
        context: &ResolvedUserContext,
        task_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let mut tx = self.db.pool().begin().await?;
        self.lock_task(&mut tx, context, task_id).await?;
        let changed = sqlx::query("UPDATE task_runs SET state='cancelled',wait_reason=NULL,lease_owner=NULL,lease_expires_at=NULL,completed_at=now(),updated_at=now() WHERE task_id=$1 AND state IN ('queued','running','waiting')").bind(task_id).execute(&mut *tx).await?.rows_affected();
        if changed != 1 {
            return Err(DurableTaskError::Conflict);
        }
        sqlx::query("UPDATE tasks SET status='cancelled',cancellation_requested_at=now(),completed_at=now(),updated_at=now() WHERE id=$1").bind(task_id).execute(&mut *tx).await?;
        tx.commit().await?;
        self.load(context, task_id).await
    }

    pub async fn recover_expired(&self, now: DateTime<Utc>) -> Result<u64, DurableTaskError> {
        Ok(sqlx::query("UPDATE task_runs SET state='queued',lease_owner=NULL,lease_expires_at=NULL,updated_at=now() WHERE state='running' AND lease_expires_at <= $1").bind(now).execute(self.db.pool()).await?.rows_affected())
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
        let row = sqlx::query("WITH candidate AS (SELECT r.id FROM task_runs r JOIN tasks t ON t.id=r.task_id WHERE (r.state='queued' OR (r.state='running' AND r.lease_expires_at <= $1)) AND t.status <> 'cancelled' ORDER BY r.created_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE task_runs r SET state='running',lease_owner=$2,lease_expires_at=$3,started_at=COALESCE(started_at,$1),updated_at=now() FROM candidate WHERE r.id=candidate.id RETURNING r.id,r.task_id,r.checkpoint")
            .bind(now).bind(worker).bind(now + lease).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(None);
        };
        let run_id: Uuid = row.get("id");
        let task_id: Uuid = row.get("task_id");
        let checkpoint: Value = row.get("checkpoint");
        sqlx::query("UPDATE tasks SET status='executing',updated_at=now() WHERE id=$1")
            .bind(task_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        let context = self.context_for_task(task_id).await?;
        let mut task = self.load(&context, task_id).await?;
        task.run_id = run_id;
        task.state = RunState::Running;
        Ok(Some(ClaimedRun { task, checkpoint }))
    }

    async fn assert_selected(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        context: &ResolvedUserContext,
        agent: &str,
    ) -> Result<(), DurableTaskError> {
        let agent = text(agent, 255)?;
        let exists=sqlx::query_scalar::<_,bool>("SELECT EXISTS(SELECT 1 FROM agent_definitions a JOIN deployment_agent_selections s ON s.agent_definition_id=a.id WHERE a.deployment_id=$1 AND a.external_key=$2 AND a.state='enabled')").bind(context.subject.deployment_id.0).bind(agent).fetch_one(&mut **tx).await?;
        if exists {
            Ok(())
        } else {
            Err(DurableTaskError::NotFound)
        }
    }

    async fn lock_task(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        context: &ResolvedUserContext,
        task_id: Uuid,
    ) -> Result<(), DurableTaskError> {
        let exists = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM tasks WHERE id=$1 AND user_context_id=$2 FOR UPDATE",
        )
        .bind(task_id)
        .bind(context.id.0)
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
        task_id: Uuid,
    ) -> Result<ResolvedUserContext, DurableTaskError> {
        let row=sqlx::query("SELECT uc.id,uc.user_id,uc.deployment_id,uc.host_app_id,uc.organization_id,uc.host_user_id FROM tasks t JOIN user_contexts uc ON uc.id=t.user_context_id WHERE t.id=$1").bind(task_id).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
        Ok(ResolvedUserContext {
            id: crate::identity::UserContextId(row.get(0)),
            user_id: crate::identity::UserId(row.get(1)),
            subject: crate::identity::UserContextSubject {
                deployment_id: crate::identity::DeploymentId(row.get(2)),
                host_app_id: crate::identity::HostAppId(row.get(3)),
                organization_id: row
                    .get::<Option<Uuid>, _>(4)
                    .map(crate::identity::HostOrganizationId),
                host_user_id: row.get(5),
            },
        })
    }

    async fn load(
        &self,
        context: &ResolvedUserContext,
        task_id: Uuid,
    ) -> Result<DurableTask, DurableTaskError> {
        let row=sqlx::query("SELECT t.title,r.id,r.state,r.wait_reason FROM tasks t JOIN task_runs r ON r.task_id=t.id WHERE t.id=$1 AND t.user_context_id=$2 ORDER BY r.created_at DESC LIMIT 1").bind(task_id).bind(context.id.0).fetch_optional(self.db.pool()).await?.ok_or(DurableTaskError::NotFound)?;
        Ok(DurableTask {
            id: task_id,
            title: row.get(0),
            run_id: row.get(1),
            state: state(&row.get::<String, _>(2))?,
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

fn state(v: &str) -> Result<RunState, DurableTaskError> {
    match v {
        "queued" => Ok(RunState::Queued),
        "running" => Ok(RunState::Running),
        "waiting" => Ok(RunState::Waiting),
        "completed" => Ok(RunState::Completed),
        "cancelled" => Ok(RunState::Cancelled),
        "failed" => Ok(RunState::Failed),
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
