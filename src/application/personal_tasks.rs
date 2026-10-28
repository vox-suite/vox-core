//! Canonical human to-dos on Timeline spans. This service never starts execution.
use crate::{
    application::spans::{SpanService, SpanServiceError},
    domain::{
        identity::Actor,
        spans::{ExecutionType, NewSpan, Span, SpanStatus},
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreatePersonalTask {
    pub request_id: String,
    pub title: String,
    #[serde(default)]
    pub notes: String,
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_links: Vec<TaskSourceLink>,
}
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskSourceLink {
    pub kind: TaskSourceKind,
    pub id: Uuid,
    #[serde(default)]
    pub evidence_ids: Vec<String>,
}
#[derive(Debug, Clone, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskSourceKind {
    Meeting,
    Event,
}
#[derive(Debug, thiserror::Error)]
pub enum PersonalTaskError {
    #[error("{0}")]
    Invalid(&'static str),
    #[error("This request ID already belongs to a different task request")]
    Conflict,
    #[error("This task was deleted; use a new request ID only for a new intentional task")]
    Deleted,
    #[error("Task storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("Task validation failed")]
    Span(#[from] SpanServiceError),
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct PersonalTaskResult {
    pub task: Span,
    pub created: bool,
}
fn validate(mut input: CreatePersonalTask) -> Result<CreatePersonalTask, PersonalTaskError> {
    if input.request_id.is_empty()
        || input.request_id.len() > 200
        || !input
            .request_id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_:".contains(&c))
    {
        return Err(PersonalTaskError::Invalid(
            "Use a request ID of 1–200 ASCII letters, digits, hyphens, underscores or colons",
        ));
    }
    input.title = input.title.trim().to_owned();
    input.notes = input.notes.trim().to_owned();
    if input.title.chars().any(char::is_control)
        || input
            .notes
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t' && c != '\r')
    {
        return Err(PersonalTaskError::Invalid(
            "Task text contains unsupported control characters",
        ));
    }
    if input.title.is_empty()
        || input.title.chars().count() > 200
        || input.notes.len() > 16_000
        || input.source_links.len() > 8
    {
        return Err(PersonalTaskError::Invalid(
            "Task title, notes or source links exceed their limits",
        ));
    }
    for link in &input.source_links {
        if link.id.is_nil()
            || link.evidence_ids.len() > 32
            || link
                .evidence_ids
                .iter()
                .any(|v| v.is_empty() || v.len() > 256 || v.chars().any(char::is_control))
        {
            return Err(PersonalTaskError::Invalid(
                "Invalid task evidence reference",
            ));
        }
    }
    Ok(input)
}
fn as_span(input: &CreatePersonalTask) -> NewSpan {
    NewSpan {
        title: input.title.clone(),
        notes: input.notes.clone(),
        category: Some("task".into()),
        source: Some("personal_task".into()),
        status: Some(SpanStatus::Planned),
        execution_type: Some(ExecutionType::ManualHuman),
        due_at: input.due_at,
        data: Some(
            serde_json::json!({"origin":"personal_task","request_id":input.request_id,"source_links":input.source_links}),
        ),
        ..Default::default()
    }
}
#[derive(Clone)]
pub struct PersonalTaskService {
    pool: PgPool,
    spans: SpanService,
}
impl PersonalTaskService {
    pub fn new(pool: PgPool, spans: SpanService) -> Self {
        Self { pool, spans }
    }
    pub async fn create(
        &self,
        actor: &Actor,
        input: CreatePersonalTask,
    ) -> Result<PersonalTaskResult, PersonalTaskError> {
        let input = validate(input)?;
        let encoded = serde_json::to_vec(&input)
            .map_err(|_| PersonalTaskError::Invalid("Invalid task request"))?;
        let digest = hex::encode(Sha256::digest(encoded));
        let mut tx = self.pool.begin().await?;
        // Unique owner+request insertion is also the concurrency gate: another
        // retry waits for this transaction to either commit or roll back.
        let inserted=sqlx::query("INSERT INTO personal_task_requests(user_id,request_id,payload_hash) VALUES($1,$2,$3) ON CONFLICT(user_id,request_id) DO NOTHING")
          .bind(actor.user_id).bind(&input.request_id).bind(&digest).execute(&mut *tx).await?.rows_affected()==1;
        let id = if inserted {
            let id = self
                .spans
                .create_span_in_transaction(actor, &mut tx, as_span(&input))
                .await?;
            sqlx::query(
                "UPDATE personal_task_requests SET span_id=$1 WHERE user_id=$2 AND request_id=$3",
            )
            .bind(id)
            .bind(actor.user_id)
            .bind(&input.request_id)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            let row=sqlx::query("SELECT payload_hash,span_id FROM personal_task_requests WHERE user_id=$1 AND request_id=$2").bind(actor.user_id).bind(&input.request_id).fetch_one(&mut *tx).await?;
            if row.get::<String, _>("payload_hash") != digest {
                return Err(PersonalTaskError::Conflict);
            }
            row.get::<Option<Uuid>, _>("span_id")
                .ok_or(PersonalTaskError::Deleted)?
        };
        tx.commit().await?;
        let task = self
            .spans
            .get_span(actor, id)
            .await?
            .ok_or(PersonalTaskError::Deleted)?;
        if inserted {
            self.spans.notify_created(actor.user_id, id);
        }
        Ok(PersonalTaskResult {
            task,
            created: inserted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn direct_tasks_need_no_meeting_or_date_and_are_manual() {
        let input: CreatePersonalTask = serde_json::from_value(
            serde_json::json!({"request_id":"request-1","title":" Send proposal "}),
        )
        .unwrap();
        let clean = validate(input).unwrap();
        assert_eq!(clean.title, "Send proposal");
        assert!(clean.due_at.is_none());
        assert!(clean.source_links.is_empty());
        let span = as_span(&clean);
        assert_eq!(span.status, Some(SpanStatus::Planned));
        assert_eq!(span.execution_type, Some(ExecutionType::ManualHuman));
        assert!(span.start_at.is_none());
        assert!(span.source_event_id.is_none());
    }
    #[test]
    fn reject_empty_titles_oversized_requests_and_execution_fields() {
        assert!(
            validate(
                serde_json::from_value(serde_json::json!({"request_id":"x","title":" "})).unwrap()
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreatePersonalTask>(
                serde_json::json!({"request_id":"x","title":"Hi","execution_type":"autonomous"})
            )
            .is_err()
        );
        assert!(
            serde_json::from_value::<CreatePersonalTask>(
                serde_json::json!({"request_id":"x","title":"Hi","due_at":"2026-10-28T10:00:00"})
            )
            .is_err()
        );
    }
    #[test]
    fn timezone_offsets_normalize_and_distinct_request_ids_remain_distinct() {
        let a=validate(serde_json::from_value(serde_json::json!({"request_id":"one","title":"Call","due_at":"2026-10-29T10:00:00+05:30"})).unwrap()).unwrap();
        let b=validate(serde_json::from_value(serde_json::json!({"request_id":"two","title":"Call","due_at":"2026-10-29T04:30:00Z"})).unwrap()).unwrap();
        assert_eq!(a.due_at, b.due_at);
        assert_ne!(a.request_id, b.request_id);
    }
}

#[cfg(test)]
mod database_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires dedicated INTEGRATION_TEST_DATABASE_URL; never use production"]
    async fn postgres_concurrent_retries_ownership_edits_and_deletion_tombstones() {
        let pool = PgPool::connect(
            &std::env::var("INTEGRATION_TEST_DATABASE_URL")
                .expect("dedicated test database required"),
        )
        .await
        .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        async fn user(pool: &PgPool) -> Actor {
            let id = sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(pool)
                .await
                .unwrap();
            sqlx::query("INSERT INTO user_contexts(deployment_id,host_app_id,host_user_id,user_id) SELECT d.id,h.id,$1::text,$1 FROM platform_deployments d JOIN host_apps h ON h.deployment_id=d.id WHERE d.external_key='vox.standalone.deployment' AND h.external_key='vox.standalone.web'").bind(id).execute(pool).await.unwrap();
            Actor::user(id)
        }
        let a = user(&pool).await;
        let b = user(&pool).await;
        let spans = SpanService::new(
            crate::storage::spans::SpanRepository::new(pool.clone()),
            crate::realtime::UserEventHub::new(),
        );
        let service = PersonalTaskService::new(pool.clone(), spans.clone());
        let input: CreatePersonalTask = serde_json::from_value(
            serde_json::json!({"request_id":"concurrent-request","title":"Send proposal"}),
        )
        .unwrap();
        let results =
            futures_util::future::join_all((0..8).map(|_| service.create(&a, input.clone()))).await;
        let tasks: Vec<_> = results.into_iter().map(Result::unwrap).collect();
        let id = tasks[0].task.id;
        assert!(tasks.iter().all(|r| r.task.id == id));
        assert_eq!(tasks.iter().filter(|r| r.created).count(), 1);
        assert!(tasks[0].task.due_at.is_none());
        assert!(tasks[0].task.start_at.is_none());
        assert_eq!(
            tasks[0].task.execution_type,
            Some(ExecutionType::ManualHuman)
        );
        let listed = spans
            .list_spans(&a, &crate::domain::spans::SpanQuery::default())
            .await
            .unwrap();
        assert_eq!(listed.iter().filter(|task| task.id == id).count(), 1);
        let own_b = service.create(&b, input.clone()).await.unwrap();
        assert_ne!(own_b.task.id, id);
        assert!(spans.get_span(&b, id).await.unwrap().is_none());
        assert!(!spans.delete_span(&b, id).await.unwrap());
        let mut conflicting = input.clone();
        conflicting.title = "Different task".into();
        assert!(matches!(
            service.create(&a, conflicting).await,
            Err(PersonalTaskError::Conflict)
        ));
        let modified = spans
            .update_span(
                &a,
                id,
                crate::domain::spans::SpanPatch {
                    expected_version: Some(tasks[0].task.version),
                    notes: Some("Edited by user".into()),
                    status: Some(SpanStatus::Done),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let replay = service.create(&a, input.clone()).await.unwrap();
        assert_eq!(replay.task.notes, "Edited by user");
        assert_eq!(replay.task.status, SpanStatus::Done);
        assert_eq!(replay.task.version, modified.version);
        assert!(spans.delete_span(&a, id).await.unwrap());
        assert!(matches!(
            service.create(&a, input.clone()).await,
            Err(PersonalTaskError::Deleted)
        ));
        let receipt: Option<Uuid> = sqlx::query_scalar(
            "SELECT span_id FROM personal_task_requests WHERE user_id=$1 AND request_id=$2",
        )
        .bind(a.user_id)
        .bind(&input.request_id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(receipt.is_none());
        let mut intentional = input;
        intentional.request_id = "new-intentional-request".into();
        let new = service.create(&a, intentional).await.unwrap();
        assert_ne!(new.task.id, id);
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(vec![a.user_id, b.user_id])
            .execute(&pool)
            .await
            .unwrap();
    }
}

#[cfg(test)]
mod input_limits_tests {
    use super::*;
    fn input(title: String, notes: String) -> CreatePersonalTask {
        CreatePersonalTask {
            request_id: "bounded".into(),
            title,
            notes,
            due_at: None,
            source_links: vec![],
        }
    }
    #[test]
    fn bounds_and_database_unsupported_control_characters_are_rejected() {
        assert!(validate(input("x".repeat(201), String::new())).is_err());
        assert!(validate(input("Task".into(), "x".repeat(16001))).is_err());
        assert!(validate(input("Bad\0title".into(), String::new())).is_err());
        assert!(validate(input("Task".into(), "Bad\0notes".into())).is_err());
        assert!(
            validate(input(
                "Task".into(),
                "First line\nSecond line\twith tab".into()
            ))
            .is_ok()
        );
    }
}
