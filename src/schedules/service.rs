/**
* Schedule registration and database persistence service.
*/
use super::{
    CreateScheduleRequest, ScheduleId, ScheduleKind, ScheduleResponse, UpdateScheduleRequest,
};
use crate::{
    db::Db,
    identity::{IdentityError, IdentityService},
};
use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use sqlx::Row;
use std::str::FromStr;

#[derive(Clone)]
pub struct ScheduleService {
    db: Db,
    identities: IdentityService,
}

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("invalid schedule request")]
    Invalid,
    #[error("schedule not found")]
    NotFound,
    #[error("schedule storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("schedule identity unavailable")]
    Identity(#[from] IdentityError),
}

impl ScheduleService {
    pub fn new(db: Db) -> Self {
        Self {
            identities: IdentityService::new(db.clone()),
            db,
        }
    }

    pub async fn create(
        &self,
        request: CreateScheduleRequest,
    ) -> Result<ScheduleResponse, ScheduleError> {
        self.create_at(request, Utc::now()).await
    }

    pub async fn create_at(
        &self,
        request: CreateScheduleRequest,
        now: DateTime<Utc>,
    ) -> Result<ScheduleResponse, ScheduleError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.instruction.trim().is_empty()
            || request.timezone.trim().is_empty()
        {
            return Err(ScheduleError::Invalid);
        }
        let timezone = Tz::from_str(request.timezone.trim()).map_err(|_| ScheduleError::Invalid)?;
        let owner = self
            .identities
            .resolve_legacy_owner(&request.identity)
            .await?;
        let (next_run_at, recurrence_expression) = match request.schedule_kind {
            ScheduleKind::Once => {
                if request.recurrence_expression.is_some() {
                    return Err(ScheduleError::Invalid);
                }
                let run_at = request.run_at.ok_or(ScheduleError::Invalid)?;
                if run_at <= now {
                    return Err(ScheduleError::Invalid);
                }
                (run_at, None)
            }
            ScheduleKind::Recurring => {
                if request.run_at.is_some() {
                    return Err(ScheduleError::Invalid);
                }
                let expression = request
                    .recurrence_expression
                    .filter(|value| !value.trim().is_empty())
                    .ok_or(ScheduleError::Invalid)?;
                (
                    compute_next_recurring(&expression, timezone, now)?,
                    Some(expression),
                )
            }
        };
        let row = sqlx::query(
            "INSERT INTO schedules (user_id, instruction, kind, recurrence_expression, timezone, next_run_at, state) \
             VALUES ($1, $2, $3, $4, $5, $6, 'active') \
             RETURNING id, instruction, kind AS schedule_kind, recurrence_expression, timezone, next_run_at, state",
        )
        .bind(owner.user_id.0)
        .bind(request.instruction.trim())
        .bind(request.schedule_kind.as_str())
        .bind(recurrence_expression)
        .bind(request.timezone.trim())
        .bind(next_run_at)
        .fetch_one(self.db.pool())
        .await?;
        schedule_response(row)
    }

    pub async fn update(
        &self,
        id: ScheduleId,
        request: UpdateScheduleRequest,
    ) -> Result<ScheduleResponse, ScheduleError> {
        self.update_at(id, request, Utc::now()).await
    }

    pub async fn update_at(
        &self,
        id: ScheduleId,
        request: UpdateScheduleRequest,
        now: DateTime<Utc>,
    ) -> Result<ScheduleResponse, ScheduleError> {
        let operation_count = usize::from(request.state.is_some())
            + usize::from(request.run_at.is_some())
            + usize::from(request.recurrence_expression.is_some());
        if operation_count != 1 {
            return Err(ScheduleError::Invalid);
        }
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
        {
            return Err(ScheduleError::Invalid);
        }
        let owner = self
            .identities
            .resolve_legacy_owner(&request.identity)
            .await?;
        let row = sqlx::query(
            "SELECT kind AS schedule_kind, recurrence_expression, timezone, next_run_at, state \
             FROM schedules \
             WHERE id = $1 AND user_id = $2",
        )
        .bind(id.0)
        .bind(owner.user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ScheduleError::NotFound)?;
        let schedule_kind =
            ScheduleKind::parse(row.get("schedule_kind")).ok_or(ScheduleError::Invalid)?;
        let timezone_name: String = row.get("timezone");
        let timezone = Tz::from_str(&timezone_name).map_err(|_| ScheduleError::Invalid)?;
        let mut state: String = row.get("state");
        let mut next_run_at: Option<DateTime<Utc>> = row.get("next_run_at");
        let mut recurrence_expression: Option<String> = row.get("recurrence_expression");

        if let Some(new_state) = request.state {
            if state == "completed" {
                return Err(ScheduleError::Invalid);
            }
            state = match new_state.trim().to_lowercase().as_str() {
                "active" => "active".into(),
                "paused" => "paused".into(),
                _ => return Err(ScheduleError::Invalid),
            };
        }
        if let Some(run_at) = request.run_at {
            if schedule_kind != ScheduleKind::Once || run_at <= now {
                return Err(ScheduleError::Invalid);
            }
            next_run_at = Some(run_at);
        }
        if let Some(expression) = request.recurrence_expression {
            if schedule_kind != ScheduleKind::Recurring || expression.trim().is_empty() {
                return Err(ScheduleError::Invalid);
            }
            next_run_at = Some(compute_next_recurring(&expression, timezone, now)?);
            recurrence_expression = Some(expression);
        }
        let row = sqlx::query(
            "UPDATE schedules SET state = $1, next_run_at = $2, recurrence_expression = $3, \
                    updated_at = now() \
             WHERE id = $4 AND user_id = $5 \
             RETURNING id, instruction, kind AS schedule_kind, recurrence_expression, timezone, next_run_at, state",
        )
        .bind(state)
        .bind(next_run_at)
        .bind(recurrence_expression)
        .bind(id.0)
        .bind(owner.user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ScheduleError::NotFound)?;
        schedule_response(row)
    }
}

fn schedule_response(row: sqlx::postgres::PgRow) -> Result<ScheduleResponse, ScheduleError> {
    Ok(ScheduleResponse {
        id: ScheduleId(row.get("id")),
        instruction: row.get("instruction"),
        schedule_kind: ScheduleKind::parse(row.get("schedule_kind"))
            .ok_or(ScheduleError::Invalid)?,
        recurrence_expression: row.get("recurrence_expression"),
        timezone: row.get("timezone"),
        next_run_at: row.get("next_run_at"),
        state: row.get("state"),
    })
}

pub fn compute_next_recurring(
    expression: &str,
    timezone: Tz,
    now: DateTime<Utc>,
) -> Result<DateTime<Utc>, ScheduleError> {
    let schedule = Schedule::from_str(expression.trim()).map_err(|_| ScheduleError::Invalid)?;
    schedule
        .after(&now.with_timezone(&timezone))
        .next()
        .map(|next| next.with_timezone(&Utc))
        .ok_or(ScheduleError::Invalid)
}
