/**
* Background ticker polling and triggering due schedules.
*/
use super::service::compute_next_recurring;
use crate::db::Db;
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use sqlx::Row;
use std::str::FromStr;
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
            "SELECT id, user_id, instruction, kind, recurrence_expression, timezone, next_run_at \
             FROM schedules \
             WHERE state = 'active' AND next_run_at <= $1 \
             FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        let count = due_schedules.len();

        for row in due_schedules {
            let id: Uuid = row.get("id");
            let user_id: Uuid = row.get("user_id");
            let schedule_kind: String = row.get("kind");
            let occurrence_at: DateTime<Utc> = row.get("next_run_at");
            let recurrence_expr: Option<String> = row.get("recurrence_expression");
            let timezone_name: String = row.get("timezone");

            sqlx::query(
                "INSERT INTO jobs (user_id, kind, payload_reference_id, schedule_id, occurrence_at, dedupe_key) \
                 SELECT $1, 'run_schedule', $2, $2, $3, $4 \
                 WHERE NOT EXISTS (\
                     SELECT 1 FROM jobs \
                     WHERE schedule_id = $2 AND occurrence_at = $3\
                 )",
            )
            .bind(user_id)
            .bind(id)
            .bind(occurrence_at)
            .bind(format!("run_schedule:{id}:{occurrence_at}"))
            .execute(&mut *tx)
            .await?;

            if schedule_kind == "once" {
                sqlx::query(
                    "UPDATE schedules SET state = 'completed', next_run_at = NULL, updated_at = now() \
                     WHERE id = $1",
                )
                .bind(id)
                .execute(&mut *tx)
                .await?;
            } else if schedule_kind == "recurring" {
                let next = if let Some(expr) = recurrence_expr {
                    let timezone = Tz::from_str(&timezone_name)
                        .map_err(|_| sqlx::Error::Decode("invalid schedule timezone".into()))?;
                    compute_next_recurring(&expr, timezone, occurrence_at)
                        .map_err(|_| sqlx::Error::Decode("invalid recurrence expression".into()))?
                } else {
                    return Err(sqlx::Error::Decode("missing recurrence expression".into()).into());
                };
                sqlx::query(
                    "UPDATE schedules SET next_run_at = $1, updated_at = now() WHERE id = $2",
                )
                .bind(next)
                .bind(id)
                .execute(&mut *tx)
                .await?;
            }
        }

        let due_tasks = sqlx::query(
            "SELECT id, user_id FROM spans \
             WHERE status = 'planned' AND execution_type = 'autonomous' AND due_at IS NOT NULL AND due_at <= $1 \
             FOR UPDATE SKIP LOCKED",
        )
        .bind(now)
        .fetch_all(&mut *tx)
        .await?;

        for row in due_tasks {
            let span_id: Uuid = row.get("id");
            let user_id: Uuid = row.get("user_id");
            sqlx::query(
                "INSERT INTO jobs (user_id, kind, payload_reference_id, span_id) \
                 VALUES ($1, 'execute_span', $2, $2) \
                 ON CONFLICT DO NOTHING",
            )
            .bind(user_id)
            .bind(span_id)
            .execute(&mut *tx)
            .await?;

            sqlx::query("UPDATE spans SET status = 'active', updated_at = now() WHERE id = $1")
                .bind(span_id)
                .execute(&mut *tx)
                .await?;
        }

        tx.commit().await?;
        Ok(count)
    }
}
