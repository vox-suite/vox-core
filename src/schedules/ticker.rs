use super::service::compute_next_recurring;
use crate::db::Db;
use chrono::{DateTime, Utc};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct ScheduleTicker {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum TickerError {
    #[error("ticker database error")]
    Database(#[from] sqlx::Error),
}

impl ScheduleTicker {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn tick(&self, now: DateTime<Utc>) -> Result<usize, TickerError> {
        let mut tx = self.db.pool().begin().await?;

        let due_schedules = sqlx::query(
            "SELECT id, instruction, schedule_kind, recurrence_expression, timezone, next_run_at \
             FROM scheduled_tasks \
             WHERE state = 'active' AND next_run_at <= $1 \
             FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        let count = due_schedules.len();

        for row in due_schedules {
            let id: Uuid = row.get("id");
            let schedule_kind: String = row.get("schedule_kind");
            let occurrence_at: DateTime<Utc> = row.get("next_run_at");
            let recurrence_expr: Option<String> = row.get("recurrence_expression");

            // Create unique RunSchedule job for this occurrence
            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id, schedule_id, occurrence_at) \
                 VALUES ('run_schedule', $1, $1, $2) \
                 ON CONFLICT (schedule_id, occurrence_at) DO NOTHING",
            )
            .bind(id)
            .bind(occurrence_at)
            .execute(&mut *tx)
            .await?;

            if schedule_kind == "once" {
                sqlx::query(
                    "UPDATE scheduled_tasks SET state = 'completed', next_run_at = NULL, updated_at = now() \
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            } else if schedule_kind == "recurring" {
                let next = if let Some(expr) = recurrence_expr {
                    compute_next_recurring(&expr, occurrence_at)
                        .unwrap_or(occurrence_at + chrono::Duration::days(1))
                } else {
                    occurrence_at + chrono::Duration::days(1)
                };
                sqlx::query(
                    "UPDATE scheduled_tasks SET next_run_at = $1, updated_at = now() WHERE id = $2",
                )
                .bind(next)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(count)
    }
}
