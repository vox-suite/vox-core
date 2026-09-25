use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineSource {
    Task,
    Schedule,
    Reminder,
    Device,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineKind {
    Completed,
    Scheduled,
    Overdue,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TimelineEntry {
    pub id: Uuid,
    pub source: TimelineSource,
    pub title: String,
    pub status: String,
    pub kind: TimelineKind,
    pub start_at: DateTime<Utc>,
    pub end_at: Option<DateTime<Utc>>,
    pub collection_id: Option<Uuid>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, thiserror::Error)]
pub enum TimelineError {
    #[error("invalid timeline request: {0}")]
    Invalid(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct TimelineService {
    db: Db,
}

impl TimelineService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn list(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
        types: Option<&[TimelineSource]>,
    ) -> Result<Vec<TimelineEntry>, TimelineError> {
        if to < from {
            return Err(TimelineError::Invalid("to must not precede from".into()));
        }

        let wants = |source: TimelineSource| match types {
            None => true,
            Some(t) => t.contains(&source),
        };

        let mut entries = Vec::new();

        if wants(TimelineSource::Task) {
            entries.extend(self.list_tasks(user_id, from, to).await?);
        }
        if wants(TimelineSource::Schedule) {
            entries.extend(self.list_schedules(user_id, from, to).await?);
        }
        if wants(TimelineSource::Reminder) {
            entries.extend(self.list_reminders(user_id, from, to).await?);
        }
        if wants(TimelineSource::Device) {
            entries.extend(self.list_device_entries(user_id, from, to).await?);
        }

        entries.sort_by_key(|e| e.start_at);
        Ok(entries)
    }

    async fn list_tasks(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TimelineEntry>, TimelineError> {
        let rows = sqlx::query(
            r#"
            SELECT id, title, status, execution_type, collection_id, due_at, created_at, completed_at
            FROM tasks
            WHERE user_id = $1
              AND COALESCE(completed_at, due_at, created_at) BETWEEN $2 AND $3
            ORDER BY created_at
            "#,
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.db.pool())
        .await?;

        let now = Utc::now();
        Ok(rows
            .into_iter()
            .map(|row| {
                let status: String = row.get("status");
                let execution_type: String = row.get("execution_type");
                let due_at: Option<DateTime<Utc>> = row.get("due_at");
                let completed_at: Option<DateTime<Utc>> = row.get("completed_at");
                let created_at: DateTime<Utc> = row.get("created_at");

                let kind = if status == "completed" {
                    TimelineKind::Completed
                } else {
                    match due_at {
                        Some(d) if d < now => TimelineKind::Overdue,
                        _ => TimelineKind::Scheduled,
                    }
                };

                TimelineEntry {
                    id: row.get("id"),
                    source: TimelineSource::Task,
                    title: row.get("title"),
                    status,
                    kind,
                    start_at: created_at,
                    end_at: completed_at.or(due_at),
                    collection_id: row.get("collection_id"),
                    metadata: serde_json::json!({ "execution_type": execution_type }),
                }
            })
            .collect())
    }

    async fn list_schedules(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TimelineEntry>, TimelineError> {
        let rows = sqlx::query(
            r#"
            SELECT id, instruction, next_run_at, state
            FROM schedules
            WHERE user_id = $1
              AND state = 'active'
              AND next_run_at BETWEEN $2 AND $3
            ORDER BY next_run_at
            "#,
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| TimelineEntry {
                id: row.get("id"),
                source: TimelineSource::Schedule,
                title: row.get("instruction"),
                status: row.get("state"),
                kind: TimelineKind::Scheduled,
                start_at: row.get("next_run_at"),
                end_at: None,
                collection_id: None,
                metadata: serde_json::json!({}),
            })
            .collect())
    }

    async fn list_reminders(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TimelineEntry>, TimelineError> {
        let rows = sqlx::query(
            r#"
            SELECT r.id, r.title, r.status, r.next_trigger_at, r.delivered_at, r.last_attempt_at, r.created_at
            FROM reminders r
            JOIN user_contexts uc ON uc.id = r.user_context_id
            WHERE uc.user_id = $1
              AND COALESCE(r.delivered_at, r.last_attempt_at, r.next_trigger_at, r.created_at) BETWEEN $2 AND $3
            ORDER BY r.created_at
            "#,
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let status: String = row.get("status");
                let next_trigger_at: Option<DateTime<Utc>> = row.get("next_trigger_at");
                let delivered_at: Option<DateTime<Utc>> = row.get("delivered_at");
                let last_attempt_at: Option<DateTime<Utc>> = row.get("last_attempt_at");
                let created_at: DateTime<Utc> = row.get("created_at");

                let kind = match status.as_str() {
                    "delivered_to_channel" => TimelineKind::Completed,
                    "missed" | "failed" => TimelineKind::Overdue,
                    _ => TimelineKind::Scheduled,
                };

                let start_at = delivered_at
                    .or(last_attempt_at)
                    .or(next_trigger_at)
                    .unwrap_or(created_at);

                TimelineEntry {
                    id: row.get("id"),
                    source: TimelineSource::Reminder,
                    title: row.get("title"),
                    status,
                    kind,
                    start_at,
                    end_at: None,
                    collection_id: None,
                    metadata: serde_json::json!({}),
                }
            })
            .collect())
    }

    async fn list_device_entries(
        &self,
        user_id: Uuid,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<TimelineEntry>, TimelineError> {
        let rows = sqlx::query(
            r#"
            SELECT id, category, title, kind, start_at, end_at
            FROM device_timeline_entries
            WHERE user_id = $1
              AND start_at BETWEEN $2 AND $3
            ORDER BY start_at
            "#,
        )
        .bind(user_id)
        .bind(from)
        .bind(to)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| {
                let kind_str: String = row.get("kind");
                let category: String = row.get("category");
                TimelineEntry {
                    id: row.get("id"),
                    source: TimelineSource::Device,
                    title: row.get("title"),
                    status: kind_str.clone(),
                    kind: match kind_str.as_str() {
                        "scheduled" => TimelineKind::Scheduled,
                        "overdue" => TimelineKind::Overdue,
                        _ => TimelineKind::Completed,
                    },
                    start_at: row.get("start_at"),
                    end_at: row.get("end_at"),
                    collection_id: None,
                    metadata: serde_json::json!({ "category": category }),
                }
            })
            .collect())
    }
}
