/**
 * Explicit-timezone reminders, delivery tracking, and scheduler (E40).
 *
 * Guarantees:
 * - One-time, interval, and calendar recurrence preserve timezone intent across
 *   restarts and daylight-saving boundaries.
 * - Delivery status truthfully distinguishes: scheduled, delivered_to_channel, failed, unknown, and missed.
 * - Delivered-to-channel is never represented as human seen or acknowledged.
 * - Missed reminders beyond the grace period window are never silently delivered late.
 * - Reminders never authorize another action or execute consequential writes.
 */
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use cron::Schedule;
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use std::{str::FromStr, sync::Arc};
use uuid::Uuid;

use crate::{db::Db, identity::ResolvedUserContext};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ReminderId(pub Uuid);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ReminderDeliveryId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderScheduleKind {
    OneTime,
    Interval,
    CalendarRecurrence,
}

impl ReminderScheduleKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::OneTime => "one_time",
            Self::Interval => "interval",
            Self::CalendarRecurrence => "calendar_recurrence",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "one_time" => Some(Self::OneTime),
            "interval" => Some(Self::Interval),
            "calendar_recurrence" => Some(Self::CalendarRecurrence),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderStatus {
    Scheduled,
    DeliveredToChannel,
    Failed,
    Unknown,
    Missed,
    Cancelled,
}

impl ReminderStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::DeliveredToChannel => "delivered_to_channel",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Missed => "missed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "scheduled" => Some(Self::Scheduled),
            "delivered_to_channel" => Some(Self::DeliveredToChannel),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            "missed" => Some(Self::Missed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReminderDeliveryStatus {
    DeliveredToChannel,
    Failed,
    Unknown,
    Missed,
}

impl ReminderDeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DeliveredToChannel => "delivered_to_channel",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
            Self::Missed => "missed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "delivered_to_channel" => Some(Self::DeliveredToChannel),
            "failed" => Some(Self::Failed),
            "unknown" => Some(Self::Unknown),
            "missed" => Some(Self::Missed),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CreateReminderRequest {
    pub title: String,
    pub message: String,
    pub channel: String,
    pub destination: String,
    pub timezone: String,
    pub schedule_kind: ReminderScheduleKind,
    pub run_at: Option<DateTime<Utc>>,
    pub interval_seconds: Option<i64>,
    pub recurrence_expression: Option<String>,
    pub max_retries: Option<i32>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Clone, Debug, Deserialize, Serialize, FromRow)]
pub struct Reminder {
    pub id: Uuid,
    pub user_context_id: Uuid,
    pub title: String,
    pub message: String,
    pub channel: String,
    pub destination: String,
    pub timezone: String,
    pub schedule_kind: String,
    pub run_at: Option<DateTime<Utc>>,
    pub interval_seconds: Option<i64>,
    pub recurrence_expression: Option<String>,
    pub next_trigger_at: Option<DateTime<Utc>>,
    pub status: String,
    pub retry_count: i32,
    pub max_retries: i32,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub failure_reason: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Serialize, FromRow)]
pub struct ReminderDelivery {
    pub id: Uuid,
    pub reminder_id: Uuid,
    pub scheduled_for: DateTime<Utc>,
    pub attempted_at: DateTime<Utc>,
    pub status: String,
    pub channel: String,
    pub destination: String,
    pub provider_receipt_id: Option<String>,
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReminderDeliveryOutcome {
    DeliveredToChannel {
        provider_receipt_id: Option<String>,
    },
    Failed {
        reason: String,
        retryable: bool,
    },
    Unknown {
        reason: String,
    },
    Missed {
        scheduled_for: DateTime<Utc>,
        detected_at: DateTime<Utc>,
    },
}

#[async_trait]
pub trait ReminderChannelAdapter: Send + Sync {
    async fn dispatch(
        &self,
        reminder: &Reminder,
        scheduled_for: DateTime<Utc>,
    ) -> Result<ReminderDeliveryOutcome, String>;
}

pub type DispatchedRecord = (Uuid, DateTime<Utc>, ReminderDeliveryOutcome);

/// In-memory mock adapter for deterministic testing and clock control.
#[derive(Clone, Default)]
pub struct MockReminderChannelAdapter {
    dispatched: Arc<std::sync::Mutex<Vec<DispatchedRecord>>>,
    forced_outcome: Arc<std::sync::Mutex<Option<ReminderDeliveryOutcome>>>,
}

impl MockReminderChannelAdapter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_forced_outcome(outcome: ReminderDeliveryOutcome) -> Self {
        Self {
            dispatched: Arc::new(std::sync::Mutex::new(Vec::new())),
            forced_outcome: Arc::new(std::sync::Mutex::new(Some(outcome))),
        }
    }

    pub fn set_outcome(&self, outcome: ReminderDeliveryOutcome) {
        let mut lock = self.forced_outcome.lock().unwrap();
        *lock = Some(outcome);
    }

    pub fn get_dispatched(&self) -> Vec<DispatchedRecord> {
        self.dispatched.lock().unwrap().clone()
    }
}

#[async_trait]
impl ReminderChannelAdapter for MockReminderChannelAdapter {
    async fn dispatch(
        &self,
        reminder: &Reminder,
        scheduled_for: DateTime<Utc>,
    ) -> Result<ReminderDeliveryOutcome, String> {
        let outcome = {
            let lock = self.forced_outcome.lock().unwrap();
            lock.clone()
                .unwrap_or(ReminderDeliveryOutcome::DeliveredToChannel {
                    provider_receipt_id: Some(format!("rcpt_{}", Uuid::new_v4())),
                })
        };
        self.dispatched
            .lock()
            .unwrap()
            .push((reminder.id, scheduled_for, outcome.clone()));
        Ok(outcome)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReminderError {
    #[error("invalid reminder parameters: {0}")]
    Invalid(String),
    #[error("reminder not found")]
    NotFound,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("unauthorized")]
    Unauthorized,
    #[error("reminders cannot authorize actions or execute consequential writes")]
    ActionAuthorityProhibited,
}

pub fn compute_next_calendar(
    expression: &str,
    timezone: Tz,
    after: DateTime<Utc>,
) -> Result<DateTime<Utc>, ReminderError> {
    let schedule = Schedule::from_str(expression)
        .map_err(|e| ReminderError::Invalid(format!("invalid cron recurrence expression: {e}")))?;
    let local = after.with_timezone(&timezone);
    let next_local = schedule.after(&local).next().ok_or_else(|| {
        ReminderError::Invalid("no upcoming occurrences found for recurrence expression".into())
    })?;
    Ok(next_local.with_timezone(&Utc))
}

#[derive(Debug, Clone)]
pub struct RecordDeliveryRequest {
    pub reminder_id: Uuid,
    pub status: ReminderDeliveryStatus,
    pub channel: String,
    pub destination: String,
    pub provider_receipt_id: Option<String>,
    pub failure_reason: Option<String>,
    pub attempted_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct ReminderService {
    db: Db,
}

impl ReminderService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Creates an explicit-timezone reminder bound to a resolved user context.
    ///
    /// GUARANTEES:
    /// - Rejects requests containing action or task authority.
    /// - Strictly validates timezone string with IANA database.
    /// - Validates recurrence schedule shape and computes initial trigger time.
    pub async fn create(
        &self,
        context: &ResolvedUserContext,
        request: CreateReminderRequest,
        now: DateTime<Utc>,
    ) -> Result<Reminder, ReminderError> {
        if request.title.trim().is_empty() {
            return Err(ReminderError::Invalid("title must not be empty".into()));
        }
        if request.message.trim().is_empty() {
            return Err(ReminderError::Invalid("message must not be empty".into()));
        }
        if request.channel.trim().is_empty() {
            return Err(ReminderError::Invalid("channel must not be empty".into()));
        }
        if request.destination.trim().is_empty() {
            return Err(ReminderError::Invalid(
                "destination must not be empty".into(),
            ));
        }

        // Validate timezone
        let timezone = Tz::from_str(request.timezone.trim()).map_err(|_| {
            ReminderError::Invalid(format!(
                "unknown or invalid IANA timezone: {}",
                request.timezone
            ))
        })?;

        // Guard against action authority injection
        if let Some(meta) = &request.metadata {
            let has_action = meta.get("action_id").is_some()
                || meta.get("proposal_id").is_some()
                || meta.get("execution_id").is_some()
                || meta.get("execute_consequential").is_some();
            if has_action {
                return Err(ReminderError::ActionAuthorityProhibited);
            }
        }

        let (next_trigger_at, interval_secs, recurrence_expr) = match request.schedule_kind {
            ReminderScheduleKind::OneTime => {
                let run_at = request.run_at.ok_or_else(|| {
                    ReminderError::Invalid("one_time reminders require run_at timestamp".into())
                })?;
                if run_at <= now {
                    return Err(ReminderError::Invalid(
                        "one_time reminder run_at must be in the future".into(),
                    ));
                }
                if request.interval_seconds.is_some() || request.recurrence_expression.is_some() {
                    return Err(ReminderError::Invalid(
                        "one_time reminders cannot have interval or recurrence expression".into(),
                    ));
                }
                (run_at, None, None)
            }
            ReminderScheduleKind::Interval => {
                let secs = request.interval_seconds.ok_or_else(|| {
                    ReminderError::Invalid("interval reminders require interval_seconds".into())
                })?;
                if secs <= 0 {
                    return Err(ReminderError::Invalid(
                        "interval_seconds must be positive".into(),
                    ));
                }
                if request.run_at.is_some() || request.recurrence_expression.is_some() {
                    return Err(ReminderError::Invalid(
                        "interval reminders cannot have run_at or recurrence expression".into(),
                    ));
                }
                let next = now
                    .checked_add_signed(Duration::seconds(secs))
                    .ok_or_else(|| {
                        ReminderError::Invalid(
                            "interval_seconds exceeds supported date range".into(),
                        )
                    })?;
                (next, Some(secs), None)
            }
            ReminderScheduleKind::CalendarRecurrence => {
                let expr = request.recurrence_expression.ok_or_else(|| {
                    ReminderError::Invalid(
                        "calendar_recurrence requires recurrence_expression".into(),
                    )
                })?;
                if request.run_at.is_some() || request.interval_seconds.is_some() {
                    return Err(ReminderError::Invalid(
                        "calendar_recurrence cannot have run_at or interval_seconds".into(),
                    ));
                }
                let next = compute_next_calendar(&expr, timezone, now)?;
                (next, None, Some(expr))
            }
        };

        let max_retries = request.max_retries.unwrap_or(3);
        if !(0..=10).contains(&max_retries) {
            return Err(ReminderError::Invalid(
                "max_retries must be between 0 and 10".into(),
            ));
        }
        let metadata = request.metadata.unwrap_or_else(|| serde_json::json!({}));

        let reminder = sqlx::query_as::<_, Reminder>(
            r#"
            INSERT INTO reminders (
                user_context_id, title, message, channel, destination,
                timezone, schedule_kind, run_at, interval_seconds, recurrence_expression,
                next_trigger_at, status, retry_count, max_retries, metadata
            ) VALUES (
                $1, $2, $3, $4, $5,
                $6, $7, $8, $9, $10,
                $11, 'scheduled', 0, $12, $13
            )
            RETURNING *
            "#,
        )
        .bind(context.id.0)
        .bind(request.title.trim())
        .bind(request.message.trim())
        .bind(request.channel.trim())
        .bind(request.destination.trim())
        .bind(request.timezone.trim())
        .bind(request.schedule_kind.as_str())
        .bind(request.run_at)
        .bind(interval_secs)
        .bind(recurrence_expr)
        .bind(next_trigger_at)
        .bind(max_retries)
        .bind(metadata)
        .fetch_one(self.db.pool())
        .await?;

        Ok(reminder)
    }

    /// Lists all reminders for the resolved user context.
    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Reminder>, ReminderError> {
        let reminders = sqlx::query_as::<_, Reminder>(
            "SELECT * FROM reminders WHERE user_context_id = $1 ORDER BY created_at DESC",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;

        Ok(reminders)
    }

    /// Gets a specific reminder by ID for the resolved user context.
    pub async fn get(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<Reminder, ReminderError> {
        let reminder = sqlx::query_as::<_, Reminder>(
            "SELECT * FROM reminders WHERE id = $1 AND user_context_id = $2",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ReminderError::NotFound)?;

        Ok(reminder)
    }

    /// Cancels a scheduled reminder.
    pub async fn cancel(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<Reminder, ReminderError> {
        let reminder = sqlx::query_as::<_, Reminder>(
            r#"
            UPDATE reminders
            SET status = 'cancelled', updated_at = now()
            WHERE id = $1 AND user_context_id = $2 AND status = 'scheduled'
            RETURNING *
            "#,
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ReminderError::NotFound)?;

        Ok(reminder)
    }

    /// Retrieves delivery history for a reminder.
    pub async fn get_deliveries(
        &self,
        context: &ResolvedUserContext,
        reminder_id: Uuid,
    ) -> Result<Vec<ReminderDelivery>, ReminderError> {
        // Ensure reminder belongs to user context
        let _ = self.get(context, reminder_id).await?;

        let deliveries = sqlx::query_as::<_, ReminderDelivery>(
            "SELECT * FROM reminder_deliveries WHERE reminder_id = $1 ORDER BY attempted_at DESC",
        )
        .bind(reminder_id)
        .fetch_all(self.db.pool())
        .await?;

        Ok(deliveries)
    }

    /// Records an external channel delivery outcome (e.g. from Bridge or provider callback).
    pub async fn record_delivery(
        &self,
        context: &ResolvedUserContext,
        request: RecordDeliveryRequest,
    ) -> Result<ReminderDelivery, ReminderError> {
        let reminder = self.get(context, request.reminder_id).await?;

        let delivery = sqlx::query_as::<_, ReminderDelivery>(
            r#"
            INSERT INTO reminder_deliveries (
                reminder_id, scheduled_for, attempted_at, status, channel, destination,
                provider_receipt_id, failure_reason
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            RETURNING *
            "#,
        )
        .bind(reminder.id)
        .bind(reminder.next_trigger_at.unwrap_or(request.attempted_at))
        .bind(request.attempted_at)
        .bind(request.status.as_str())
        .bind(&request.channel)
        .bind(&request.destination)
        .bind(&request.provider_receipt_id)
        .bind(&request.failure_reason)
        .fetch_one(self.db.pool())
        .await?;

        // Update reminder state
        match request.status {
            ReminderDeliveryStatus::DeliveredToChannel => {
                if reminder.schedule_kind == ReminderScheduleKind::OneTime.as_str() {
                    sqlx::query(
                        r#"
                        UPDATE reminders
                        SET status = 'delivered_to_channel', delivered_at = $1, last_attempt_at = $1, updated_at = now()
                        WHERE id = $2
                        "#,
                    )
                    .bind(request.attempted_at)
                    .bind(reminder.id)
                    .execute(self.db.pool())
                    .await?;
                }
            }
            ReminderDeliveryStatus::Failed => {
                sqlx::query(
                    r#"
                    UPDATE reminders
                    SET last_attempt_at = $1, failure_reason = $2, updated_at = now()
                    WHERE id = $3
                    "#,
                )
                .bind(request.attempted_at)
                .bind(&request.failure_reason)
                .bind(reminder.id)
                .execute(self.db.pool())
                .await?;
            }
            _ => {}
        }

        Ok(delivery)
    }
}

/// Scheduler handling due reminder processing, retry management, and missed-reminder prevention.
#[derive(Clone)]
pub struct ReminderScheduler {
    db: Db,
    adapter: Arc<dyn ReminderChannelAdapter>,
    grace_period: Duration,
}

impl ReminderScheduler {
    pub fn new(db: Db, adapter: Arc<dyn ReminderChannelAdapter>) -> Self {
        Self {
            db,
            adapter,
            grace_period: Duration::minutes(15), // Standard 15-minute grace period
        }
    }

    pub fn with_grace_period(mut self, grace_period: Duration) -> Self {
        self.grace_period = grace_period;
        self
    }

    /// Processes all reminders due at or before `now`.
    ///
    /// GUARANTEES:
    /// - Missed reminder policy: occurrences overdue beyond `grace_period` are NOT delivered late;
    ///   they are recorded as `missed`.
    /// - Status truthfully distinguishes `scheduled`, `delivered_to_channel`, `failed`, `unknown`, and `missed`.
    /// - Bounded retries: failure retries back off (1m, 5m, 15m) and terminate at `max_retries`.
    /// - Idempotent and concurrency-safe: uses row locks (`FOR UPDATE SKIP LOCKED`).
    pub async fn process_due_reminders(&self, now: DateTime<Utc>) -> Result<usize, ReminderError> {
        self.process_due_reminders_internal(now, None).await
    }

    /// Processes reminders due at or before `now` scoped to a specific user context.
    pub async fn process_due_reminders_for_context(
        &self,
        user_context_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<usize, ReminderError> {
        self.process_due_reminders_internal(now, Some(user_context_id))
            .await
    }

    async fn process_due_reminders_internal(
        &self,
        now: DateTime<Utc>,
        user_context_id: Option<Uuid>,
    ) -> Result<usize, ReminderError> {
        let mut tx = self.db.pool().begin().await?;

        let due_reminders = sqlx::query_as::<_, Reminder>(
            r#"
            SELECT * FROM reminders
            WHERE status = 'scheduled'
              AND next_trigger_at <= $1
              AND ($2::uuid IS NULL OR user_context_id = $2)
            ORDER BY next_trigger_at ASC
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(now)
        .bind(user_context_id)
        .fetch_all(&mut *tx)
        .await?;

        let count = due_reminders.len();

        for reminder in due_reminders {
            let scheduled_for = reminder.next_trigger_at.unwrap_or(now);

            // 1. MISSED REMINDER POLICY CHECK
            // If the reminder is late beyond the grace period window (e.g. server was offline),
            // it must NOT be silently delivered late.
            let delay = now - scheduled_for;
            if delay > self.grace_period {
                let failure_reason = format!(
                    "Delivery window expired; reminder occurrence was delayed by {}s (exceeding {}s grace window) and was not delivered late",
                    delay.num_seconds(),
                    self.grace_period.num_seconds()
                );

                sqlx::query(
                    r#"
                    INSERT INTO reminder_deliveries (
                        reminder_id, scheduled_for, attempted_at, status, channel, destination, failure_reason
                    ) VALUES ($1, $2, $3, 'missed', $4, $5, $6)
                    "#,
                )
                .bind(reminder.id)
                .bind(scheduled_for)
                .bind(now)
                .bind(&reminder.channel)
                .bind(&reminder.destination)
                .bind(&failure_reason)
                .execute(&mut *tx)
                .await?;

                if reminder.schedule_kind == ReminderScheduleKind::OneTime.as_str() {
                    sqlx::query(
                        r#"
                        UPDATE reminders
                        SET status = 'missed', failure_reason = $1, updated_at = now()
                        WHERE id = $2
                        "#,
                    )
                    .bind(&failure_reason)
                    .bind(reminder.id)
                    .execute(&mut *tx)
                    .await?;
                } else {
                    // For recurring/interval, advance next_trigger_at to the next future occurrence
                    let next = self.compute_next_occurrence(&reminder, now)?;
                    sqlx::query(
                        r#"
                        UPDATE reminders
                        SET next_trigger_at = $1, failure_reason = $2, retry_count = 0, updated_at = now()
                        WHERE id = $3
                        "#,
                    )
                    .bind(next)
                    .bind(&failure_reason)
                    .bind(reminder.id)
                    .execute(&mut *tx)
                    .await?;
                }
                continue;
            }

            // 2. DISPATCH TO CHANNEL ADAPTER
            let outcome = self
                .adapter
                .dispatch(&reminder, scheduled_for)
                .await
                .unwrap_or_else(|e| ReminderDeliveryOutcome::Failed {
                    reason: e,
                    retryable: true,
                });

            match outcome {
                ReminderDeliveryOutcome::DeliveredToChannel {
                    provider_receipt_id,
                } => {
                    sqlx::query(
                        r#"
                        INSERT INTO reminder_deliveries (
                            reminder_id, scheduled_for, attempted_at, status, channel, destination, provider_receipt_id
                        ) VALUES ($1, $2, $3, 'delivered_to_channel', $4, $5, $6)
                        "#,
                    )
                    .bind(reminder.id)
                    .bind(scheduled_for)
                    .bind(now)
                    .bind(&reminder.channel)
                    .bind(&reminder.destination)
                    .bind(provider_receipt_id)
                    .execute(&mut *tx)
                    .await?;

                    if reminder.schedule_kind == ReminderScheduleKind::OneTime.as_str() {
                        sqlx::query(
                            r#"
                            UPDATE reminders
                            SET status = 'delivered_to_channel', delivered_at = $1, last_attempt_at = $1, updated_at = now()
                            WHERE id = $2
                            "#,
                        )
                        .bind(now)
                        .bind(reminder.id)
                        .execute(&mut *tx)
                        .await?;
                    } else {
                        let next = self.compute_next_occurrence(&reminder, scheduled_for)?;
                        sqlx::query(
                            r#"
                            UPDATE reminders
                            SET next_trigger_at = $1, delivered_at = $2, last_attempt_at = $2, retry_count = 0, failure_reason = NULL, updated_at = now()
                            WHERE id = $3
                            "#,
                        )
                        .bind(next)
                        .bind(now)
                        .bind(reminder.id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }
                ReminderDeliveryOutcome::Failed { reason, retryable } => {
                    let next_retry = reminder.retry_count + 1;
                    if retryable && next_retry < reminder.max_retries {
                        // Bounded retry backoff: 1m, 5m, 15m
                        let backoff_secs = match next_retry {
                            1 => 60,
                            2 => 300,
                            _ => 900,
                        };
                        let retry_at = now + Duration::seconds(backoff_secs);

                        sqlx::query(
                            r#"
                            INSERT INTO reminder_deliveries (
                                reminder_id, scheduled_for, attempted_at, status, channel, destination, failure_reason
                            ) VALUES ($1, $2, $3, 'failed', $4, $5, $6)
                            "#,
                        )
                        .bind(reminder.id)
                        .bind(scheduled_for)
                        .bind(now)
                        .bind(&reminder.channel)
                        .bind(&reminder.destination)
                        .bind(format!("Attempt {next_retry} failed (retrying in {backoff_secs}s): {reason}"))
                        .execute(&mut *tx)
                        .await?;

                        sqlx::query(
                            r#"
                            UPDATE reminders
                            SET next_trigger_at = $1, retry_count = $2, last_attempt_at = $3, failure_reason = $4, updated_at = now()
                            WHERE id = $5
                            "#,
                        )
                        .bind(retry_at)
                        .bind(next_retry)
                        .bind(now)
                        .bind(&reason)
                        .bind(reminder.id)
                        .execute(&mut *tx)
                        .await?;
                    } else {
                        // Retries exhausted or non-retryable
                        sqlx::query(
                            r#"
                            INSERT INTO reminder_deliveries (
                                reminder_id, scheduled_for, attempted_at, status, channel, destination, failure_reason
                            ) VALUES ($1, $2, $3, 'failed', $4, $5, $6)
                            "#,
                        )
                        .bind(reminder.id)
                        .bind(scheduled_for)
                        .bind(now)
                        .bind(&reminder.channel)
                        .bind(&reminder.destination)
                        .bind(&reason)
                        .execute(&mut *tx)
                        .await?;

                        sqlx::query(
                            r#"
                            UPDATE reminders
                            SET status = 'failed', last_attempt_at = $1, failure_reason = $2, updated_at = now()
                            WHERE id = $3
                            "#,
                        )
                        .bind(now)
                        .bind(&reason)
                        .bind(reminder.id)
                        .execute(&mut *tx)
                        .await?;
                    }
                }
                ReminderDeliveryOutcome::Unknown { reason } => {
                    sqlx::query(
                        r#"
                        INSERT INTO reminder_deliveries (
                            reminder_id, scheduled_for, attempted_at, status, channel, destination, failure_reason
                        ) VALUES ($1, $2, $3, 'unknown', $4, $5, $6)
                        "#,
                    )
                    .bind(reminder.id)
                    .bind(scheduled_for)
                    .bind(now)
                    .bind(&reminder.channel)
                    .bind(&reminder.destination)
                    .bind(&reason)
                    .execute(&mut *tx)
                    .await?;

                    sqlx::query(
                        r#"
                        UPDATE reminders
                        SET status = 'unknown', last_attempt_at = $1, failure_reason = $2, updated_at = now()
                        WHERE id = $3
                        "#,
                    )
                    .bind(now)
                    .bind(&reason)
                    .bind(reminder.id)
                    .execute(&mut *tx)
                    .await?;
                }
                ReminderDeliveryOutcome::Missed {
                    scheduled_for,
                    detected_at,
                } => {
                    let reason =
                        format!("Missed occurrence at {scheduled_for} detected at {detected_at}");
                    sqlx::query(
                        r#"
                        INSERT INTO reminder_deliveries (
                            reminder_id, scheduled_for, attempted_at, status, channel, destination, failure_reason
                        ) VALUES ($1, $2, $3, 'missed', $4, $5, $6)
                        "#,
                    )
                    .bind(reminder.id)
                    .bind(scheduled_for)
                    .bind(detected_at)
                    .bind(&reminder.channel)
                    .bind(&reminder.destination)
                    .bind(&reason)
                    .execute(&mut *tx)
                    .await?;

                    sqlx::query(
                        r#"
                        UPDATE reminders
                        SET status = 'missed', failure_reason = $1, updated_at = now()
                        WHERE id = $2
                        "#,
                    )
                    .bind(&reason)
                    .bind(reminder.id)
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }

        tx.commit().await?;
        Ok(count)
    }

    fn compute_next_occurrence(
        &self,
        reminder: &Reminder,
        after: DateTime<Utc>,
    ) -> Result<DateTime<Utc>, ReminderError> {
        let timezone = Tz::from_str(&reminder.timezone).map_err(|_| {
            ReminderError::Invalid(format!("invalid timezone {}", reminder.timezone))
        })?;

        if reminder.schedule_kind == ReminderScheduleKind::Interval.as_str() {
            let secs = reminder.interval_seconds.unwrap_or(3600);
            after
                .checked_add_signed(Duration::seconds(secs))
                .ok_or_else(|| {
                    ReminderError::Invalid("interval_seconds exceeds supported date range".into())
                })
        } else if reminder.schedule_kind == ReminderScheduleKind::CalendarRecurrence.as_str() {
            let expr = reminder
                .recurrence_expression
                .as_ref()
                .ok_or_else(|| ReminderError::Invalid("missing recurrence expression".into()))?;
            compute_next_calendar(expr, timezone, after)
        } else {
            Err(ReminderError::Invalid(
                "one_time reminder has no next occurrence".into(),
            ))
        }
    }
}
