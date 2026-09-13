use super::{
    CreateScheduleRequest, ScheduleId, ScheduleKind, ScheduleResponse, UpdateScheduleRequest,
};
use crate::{
    db::Db,
    identity::{ChannelIdentity, UserId},
};
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct ScheduleService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("invalid schedule request")]
    Invalid,
    #[error("schedule not found")]
    NotFound,
    #[error("schedule storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl ScheduleService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn create(
        &self,
        request: CreateScheduleRequest,
    ) -> Result<ScheduleResponse, ScheduleError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.instruction.trim().is_empty()
            || request.timezone.trim().is_empty()
        {
            return Err(ScheduleError::Invalid);
        }

        let user_id = self.resolve_identity(&request.identity).await?;

        let (next_run_at, recurrence_expr) = match request.schedule_kind {
            ScheduleKind::Once => {
                let run_at = request.run_at.ok_or(ScheduleError::Invalid)?;
                if run_at <= Utc::now() {
                    return Err(ScheduleError::Invalid);
                }
                (Some(run_at), None)
            }
            ScheduleKind::Recurring => {
                let expr = request
                    .recurrence_expression
                    .filter(|e| !e.trim().is_empty())
                    .ok_or(ScheduleError::Invalid)?;
                let next = compute_next_recurring(&expr, Utc::now())?;
                (Some(next), Some(expr))
            }
        };

        let row = sqlx::query(
            "INSERT INTO scheduled_tasks (user_id, instruction, schedule_kind, recurrence_expression, timezone, next_run_at, state) \
             VALUES ($1, $2, $3, $4, $5, $6, 'active') \
             RETURNING id, instruction, schedule_kind, recurrence_expression, timezone, next_run_at, state",
        )
        .bind(user_id.0)
        .bind(request.instruction.trim())
        .bind(request.schedule_kind.as_str())
        .bind(recurrence_expr.as_deref())
        .bind(request.timezone.trim())
        .bind(next_run_at)
        .fetch_one(self.db.pool())
        .await?;

        Ok(ScheduleResponse {
            id: ScheduleId(row.get("id")),
            instruction: row.get("instruction"),
            schedule_kind: ScheduleKind::parse(row.get("schedule_kind"))
                .unwrap_or(ScheduleKind::Once),
            recurrence_expression: row.get("recurrence_expression"),
            timezone: row.get("timezone"),
            next_run_at: row.get("next_run_at"),
            state: row.get("state"),
        })
    }

    pub async fn update(
        &self,
        id: ScheduleId,
        request: UpdateScheduleRequest,
    ) -> Result<ScheduleResponse, ScheduleError> {
        let existing = sqlx::query(
            "SELECT id, instruction, schedule_kind, recurrence_expression, timezone, next_run_at, state \
             FROM scheduled_tasks WHERE id = $1",
        )
        .bind(id.0)
        .fetch_optional(self.db.pool())
        .await?;

        let existing_row = existing.ok_or(ScheduleError::NotFound)?;
        let mut state: String = existing_row.get("state");
        let mut next_run_at: Option<DateTime<Utc>> = existing_row.get("next_run_at");
        let mut recurrence_expr: Option<String> = existing_row.get("recurrence_expression");

        if let Some(new_state) = request.state {
            let s = new_state.trim().to_lowercase();
            if s != "active" && s != "paused" {
                return Err(ScheduleError::Invalid);
            }
            state = s;
        }

        if let Some(run_at) = request.run_at {
            next_run_at = Some(run_at);
        }

        if let Some(expr) = request.recurrence_expression {
            let next = compute_next_recurring(&expr, Utc::now())?;
            recurrence_expr = Some(expr);
            next_run_at = Some(next);
        }

        let updated = sqlx::query(
            "UPDATE scheduled_tasks SET state = $1, next_run_at = $2, recurrence_expression = $3, updated_at = now() \
             WHERE id = $4 \
             RETURNING id, instruction, schedule_kind, recurrence_expression, timezone, next_run_at, state",
        )
        .bind(&state)
        .bind(next_run_at)
        .bind(recurrence_expr.as_deref())
        .bind(id.0)
        .fetch_one(self.db.pool())
        .await?;

        Ok(ScheduleResponse {
            id: ScheduleId(updated.get("id")),
            instruction: updated.get("instruction"),
            schedule_kind: ScheduleKind::parse(updated.get("schedule_kind"))
                .unwrap_or(ScheduleKind::Once),
            recurrence_expression: updated.get("recurrence_expression"),
            timezone: updated.get("timezone"),
            next_run_at: updated.get("next_run_at"),
            state: updated.get("state"),
        })
    }

    async fn resolve_identity(&self, identity: &ChannelIdentity) -> Result<UserId, ScheduleError> {
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
        )
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }

        let mut tx = self.db.pool().begin().await?;
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO NOTHING RETURNING user_id",
        )
        .bind(new_user)
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(&mut *tx)
        .await?;

        let id = if let Some(id) = inserted {
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
            )
            .bind(identity.channel.trim())
            .bind(identity.external_id.trim())
            .fetch_one(&mut *tx)
            .await?
        };
        tx.commit().await?;
        Ok(UserId(id))
    }
}

pub fn compute_next_recurring(
    expr: &str,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, ScheduleError> {
    match expr.trim().to_lowercase().as_str() {
        "@hourly" | "hourly" => Ok(now + Duration::hours(1)),
        "@daily" | "daily" => Ok(now + Duration::days(1)),
        "@weekly" | "weekly" => Ok(now + Duration::weeks(1)),
        other => {
            if let Ok(minutes) = other.parse::<i64>()
                && minutes > 0
            {
                return Ok(now + Duration::minutes(minutes));
            }
            // If standard cron or unparsed, advance by 1 day as safe default
            Ok(now + Duration::days(1))
        }
    }
}
