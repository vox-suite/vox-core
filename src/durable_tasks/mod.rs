pub mod runs;
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
    Budget,
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
    pub agent_external_key: Option<String>,
    pub instruction_version: Option<i32>,
    pub result: Value,
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
        let actor =
            runs::capture_actor(&self.db, context, request.agent_external_key.as_deref()).await?;
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT id FROM user_contexts WHERE id=$1 AND user_id=$2 FOR UPDATE")
            .bind(context.id.0)
            .bind(context.user_id.0)
            .fetch_one(&mut *tx)
            .await?;
        let active:i64=sqlx::query_scalar("SELECT count(*) FROM jobs j JOIN assigned_task_runs r ON r.job_id=j.id WHERE r.user_context_id=$1 AND j.state IN ('pending','running')").bind(context.id.0).fetch_one(&mut *tx).await?;
        if active >= 8 {
            return Err(DurableTaskError::Conflict);
        }
        let span_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO spans (user_id, user_context_id, title, notes, status, execution_type) VALUES ($1, $4, $2, $3, 'planned', 'interactive') RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(title.clone())
        .bind(&instruction)
        .bind(context.id.0)
        .fetch_one(&mut *tx)
        .await?;
        let run_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO jobs (user_id, user_context_id, kind, payload_reference_id, span_id, state, checkpoint,max_attempts) VALUES ($1, $3, 'execute_span', $2, $2, 'pending', '{}',3) RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(span_id)
        .bind(context.id.0)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO assigned_task_runs(job_id,user_context_id,agent_id,instruction_version,model_configuration_id,model_version,actor_snapshot,authority,task_instruction,deadline_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,now()+interval '1 day')")
            .bind(run_id).bind(context.id.0).bind(actor.agent.definition.id).bind(actor.agent.definition.instruction_version).bind(actor.agent.model_configuration.id).bind(actor.agent.model_configuration.version)
            .bind(serde_json::to_value(&actor.agent).map_err(|_|DurableTaskError::Invalid)?).bind(serde_json::to_value(&actor.authority).map_err(|_|DurableTaskError::Invalid)?).bind(instruction).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(DurableTask {
            id: span_id,
            title,
            state: RunState::Queued,
            run_id,
            wait_reason: None,
            agent_external_key: Some(actor.agent.definition.external_key),
            instruction_version: Some(actor.agent.definition.instruction_version),
            result: serde_json::json!({}),
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
        if !request.checkpoint.is_object() || request.checkpoint.to_string().len() > 32 * 1024 {
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
        let pending:Option<Uuid>=sqlx::query_scalar("SELECT pending_proposal_id FROM assigned_task_runs WHERE job_id IN (SELECT id FROM jobs WHERE span_id=$1)").bind(span_id).fetch_optional(&mut *tx).await?.flatten();
        if let Some(proposal) = pending {
            let outcome:Option<Value>=sqlx::query_scalar("SELECT jsonb_build_object('proposal_id',p.id,'proposal_state',p.state,'execution_state',e.state,'confirmation_evidence',e.confirmation_evidence) FROM action_proposals p LEFT JOIN executions e ON e.proposal_id=p.id AND e.user_context_id=p.user_context_id WHERE p.id=$1 AND p.user_context_id=$2 AND (p.state IN ('rejected','expired') OR e.state IN ('succeeded','failed'))")
                .bind(proposal).bind(context.id.0).fetch_optional(&mut *tx).await?;
            let Some(outcome) = outcome else {
                return Err(DurableTaskError::Conflict);
            };
            sqlx::query("UPDATE jobs SET checkpoint=$2 WHERE span_id=$1")
                .bind(span_id)
                .bind(serde_json::json!({"approved_action_outcome":outcome}))
                .execute(&mut *tx)
                .await?;
            sqlx::query("UPDATE assigned_task_runs SET pending_proposal_id=NULL WHERE pending_proposal_id=$1").bind(proposal).execute(&mut *tx).await?;
        }
        let changed = sqlx::query(
            "UPDATE jobs SET state='pending', wait_reason=NULL WHERE span_id=$1 AND kind='execute_span' AND state='pending' AND wait_reason IS NOT NULL AND wait_reason <> 'budget'",
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
            "WITH candidate AS (SELECT j.id FROM jobs j JOIN spans t ON t.id=j.span_id WHERE j.kind='execute_span' AND j.wait_reason IS NULL AND ((j.state='pending' AND j.available_at <= $1) OR (j.state='running' AND j.lease_expires_at <= $1)) AND t.status <> 'cancelled' ORDER BY j.created_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE jobs j SET state='running', lease_owner=$2, lease_expires_at=$3 FROM candidate WHERE j.id=candidate.id RETURNING j.id, j.span_id, j.checkpoint",
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
            "SELECT id FROM spans WHERE id=$1 AND user_id=$2 AND user_context_id=$3 FOR UPDATE",
        )
        .bind(span_id)
        .bind(context.user_id.0)
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
            "SELECT t.title, j.id, j.state, j.wait_reason, r.actor_snapshot->'definition'->>'external_key' AS actor_key,r.instruction_version,t.execution_result FROM spans t JOIN jobs j ON j.span_id=t.id LEFT JOIN assigned_task_runs r ON r.job_id=j.id WHERE t.id=$1 AND t.user_id=$2 AND t.user_context_id=$3 AND j.user_context_id=$3 AND j.kind='execute_span' ORDER BY j.created_at DESC LIMIT 1",
        )
        .bind(span_id)
        .bind(context.user_id.0)
        .bind(context.id.0)
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
            agent_external_key: row.get("actor_key"),
            instruction_version: row.get("instruction_version"),
            result: row.get("execution_result"),
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
        "budget" => Ok(WaitReason::Budget),
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
        WaitReason::Budget => "budget",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{db::jobs::JobRepository, identity::IdentityService};

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL"]
    async fn durable_task_context_and_worker_wait_boundaries() {
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
        let service = DurableTaskService::new(db.clone());
        let task = service
            .start(
                &context,
                StartTaskRequest {
                    title: "Private assigned work".into(),
                    instruction: "Prepare a report".into(),
                    agent_external_key: None,
                },
            )
            .await
            .unwrap();
        let mut substituted = context.clone();
        substituted.id = UserContextId(Uuid::new_v4());
        assert!(matches!(
            service.get(&substituted, task.id).await,
            Err(DurableTaskError::NotFound)
        ));
        assert!(matches!(
            service
                .wait(
                    &substituted,
                    task.id,
                    WaitRequest {
                        reason: WaitReason::Approval,
                        checkpoint: serde_json::json!({})
                    }
                )
                .await,
            Err(DurableTaskError::NotFound)
        ));
        assert!(matches!(
            service.cancel(&substituted, task.id).await,
            Err(DurableTaskError::NotFound)
        ));
        assert!(
            JobRepository::new(db.clone())
                .claim("fixture", Utc::now(), Duration::seconds(30), 100)
                .await
                .unwrap()
                .iter()
                .all(|job| job.id != task.run_id)
        );
        let future = Utc::now() + Duration::hours(1);
        sqlx::query("UPDATE jobs SET available_at=$2 WHERE id=$1")
            .bind(task.run_id)
            .bind(future)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(
            service
                .claim_next("governed-fixture", Utc::now(), Duration::seconds(30))
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query("UPDATE jobs SET available_at=now() WHERE id=$1")
            .bind(task.run_id)
            .execute(db.pool())
            .await
            .unwrap();
        service
            .wait(
                &context,
                task.id,
                WaitRequest {
                    reason: WaitReason::Approval,
                    checkpoint: serde_json::json!({}),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            service.resume(&substituted, task.id).await,
            Err(DurableTaskError::NotFound)
        ));
        let repository = JobRepository::new(db.clone());
        let now = Utc::now();
        assert!(
            repository
                .claim("fixture", now, Duration::seconds(30), 100)
                .await
                .unwrap()
                .iter()
                .all(|job| job.id != task.run_id)
        );
        // Even an expired lease cannot supersede a persisted approval wait.
        sqlx::query("UPDATE jobs SET state='running', lease_owner='expired', lease_expires_at=$2 WHERE id=$1")
            .bind(task.run_id).bind(now-Duration::seconds(1)).execute(db.pool()).await.unwrap();
        assert!(
            repository
                .claim("fixture", now, Duration::seconds(30), 100)
                .await
                .unwrap()
                .iter()
                .all(|job| job.id != task.run_id)
        );
        sqlx::query(
            "UPDATE jobs SET state='pending',lease_owner=NULL,lease_expires_at=NULL WHERE id=$1",
        )
        .bind(task.run_id)
        .execute(db.pool())
        .await
        .unwrap();
        service.resume(&context, task.id).await.unwrap();
        assert_eq!(
            service.get(&context, task.id).await.unwrap().state,
            RunState::Queued
        );
        // Clearing the wait or replacing a checkpoint does not authorize the
        // legacy autonomous summarizer/phone dispatcher to execute this task.
        assert!(
            repository
                .claim("fixture", now, Duration::seconds(30), 100)
                .await
                .unwrap()
                .iter()
                .all(|job| job.id != task.run_id)
        );
        let autonomous_span: Uuid = sqlx::query_scalar("INSERT INTO spans(user_id,user_context_id,title,execution_type) VALUES($1,$2,'Explicit scheduled work','autonomous') RETURNING id")
            .bind(user).bind(context.id.0).fetch_one(db.pool()).await.unwrap();
        let autonomous_job: Uuid = sqlx::query_scalar("INSERT INTO jobs(user_id,user_context_id,kind,span_id,payload_reference_id) VALUES($1,$2,'execute_span',$3,$3) RETURNING id")
            .bind(user).bind(context.id.0).bind(autonomous_span).fetch_one(db.pool()).await.unwrap();
        let event_job = repository
            .enqueue(crate::jobs::JobKind::ProcessEvent, Uuid::new_v4())
            .await
            .unwrap();
        let eligible = repository
            .claim("fixture", Utc::now(), Duration::seconds(30), 100)
            .await
            .unwrap();
        assert!(eligible.iter().any(|job| job.id == autonomous_job));
        assert!(eligible.iter().any(|job| job.id == event_job));
        assert!(!eligible.iter().any(|job| job.id == task.run_id));
        service.cancel(&context, task.id).await.unwrap();
        assert_eq!(
            service.get(&context, task.id).await.unwrap().state,
            RunState::Cancelled
        );
        sqlx::query("DELETE FROM jobs WHERE id=$1")
            .bind(event_job)
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(db.pool())
            .await
            .unwrap();
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TaskPage {
    pub tasks: Vec<DurableTask>,
    pub next_cursor: Option<Uuid>,
}
impl DurableTaskService {
    pub async fn query(
        &self,
        context: &ResolvedUserContext,
        cursor: Option<Uuid>,
        limit: usize,
    ) -> Result<TaskPage, DurableTaskError> {
        if !(1..=50).contains(&limit) {
            return Err(DurableTaskError::Invalid);
        }
        let ids:Vec<Uuid>=sqlx::query_scalar("SELECT s.id FROM spans s WHERE s.user_id=$1 AND s.user_context_id=$2 AND ($3::uuid IS NULL OR s.id<$3) AND EXISTS(SELECT 1 FROM jobs j WHERE j.span_id=s.id AND j.kind='execute_span' AND j.user_context_id=$2) ORDER BY s.id DESC LIMIT $4")
            .bind(context.user_id.0).bind(context.id.0).bind(cursor).bind(limit as i64+1).fetch_all(self.db.pool()).await?;
        let more = ids.len() > limit;
        let mut tasks = Vec::new();
        for id in ids.into_iter().take(limit) {
            tasks.push(self.load(context, id).await?);
        }
        let next_cursor = if more {
            tasks.last().map(|task| task.id)
        } else {
            None
        };
        Ok(TaskPage { tasks, next_cursor })
    }
}
