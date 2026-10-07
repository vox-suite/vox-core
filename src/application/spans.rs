/**
* Application service for spans: validation, ownership, and change notifications.
*/
use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use uuid::Uuid;

use crate::{
    domain::{
        ConcurrencyOutcome,
        identity::Actor,
        spans::{
            NewSpan, Span, SpanDayPage, SpanDayQuery, SpanDays, SpanDaysQuery, SpanPatch, SpanQuery,
        },
    },
    realtime::UserEventHub,
    storage::spans::SpanRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum SpanServiceError {
    #[error("invalid span: {0}")]
    Invalid(&'static str),
    #[error("referenced collection or parent span not found")]
    ReferenceNotFound,
    #[error("span not found")]
    NotFound,
    #[error("optimistic concurrency conflict")]
    VersionConflict,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

const MAX_DAYS_PER_REQUEST: i64 = 62;
const DEFAULT_PAGE: i64 = 40;
const MAX_PAGE: i64 = 100;

fn day_bounds(
    day: NaiveDate,
    tz: chrono_tz::Tz,
) -> Result<(DateTime<Utc>, DateTime<Utc>), SpanServiceError> {
    let start_of = |d: NaiveDate| {
        (0..=3)
            .find_map(|h| {
                d.and_hms_opt(h, 0, 0)
                    .and_then(|local| tz.from_local_datetime(&local).earliest())
            })
            .map(|t| t.with_timezone(&Utc))
            .ok_or(SpanServiceError::Invalid("invalid day"))
    };
    let next = day
        .succ_opt()
        .ok_or(SpanServiceError::Invalid("invalid day"))?;
    Ok((start_of(day)?, start_of(next)?))
}

fn parse_timezone(name: &str) -> Result<chrono_tz::Tz, SpanServiceError> {
    name.parse()
        .map_err(|_| SpanServiceError::Invalid("unknown timezone"))
}

fn encode_cursor(span: &Span) -> Option<String> {
    let start = span.start_at?;
    Some(format!("{}_{}", start.timestamp_micros(), span.id))
}

fn decode_cursor(cursor: &str) -> Result<(DateTime<Utc>, Uuid), SpanServiceError> {
    let invalid = || SpanServiceError::Invalid("invalid cursor");
    let (micros, id) = cursor.split_once('_').ok_or_else(invalid)?;
    let start = DateTime::from_timestamp_micros(micros.parse().map_err(|_| invalid())?)
        .ok_or_else(invalid)?;
    Ok((start, id.parse().map_err(|_| invalid())?))
}

#[derive(Clone)]
pub struct SpanService {
    repo: SpanRepository,
    user_events: UserEventHub,
}

impl SpanService {
    pub fn new(repo: SpanRepository, user_events: UserEventHub) -> Self {
        Self { repo, user_events }
    }

    pub async fn create_span(
        &self,
        actor: &Actor,
        input: NewSpan,
    ) -> Result<Span, SpanServiceError> {
        if input.title.trim().is_empty() {
            return Err(SpanServiceError::Invalid("title must not be empty"));
        }
        if let (Some(start), Some(end)) = (input.start_at, input.end_at)
            && end < start
        {
            return Err(SpanServiceError::Invalid(
                "end_at must not precede start_at",
            ));
        }
        let span = self
            .repo
            .create(actor.user_id, input)
            .await
            .map_err(missing_reference)?;
        self.notify(actor.user_id, "span_created", span.id);
        Ok(span)
    }

    pub async fn update_span(
        &self,
        actor: &Actor,
        id: Uuid,
        patch: SpanPatch,
    ) -> Result<Span, SpanServiceError> {
        if let Some(span) = self.repo.get_by_id(actor.user_id, id).await?
            && matches!(
                span.source.as_str(),
                "google_calendar" | "spotify" | "youtube" | "google_maps"
            )
            && (patch.title.is_some()
                || patch.start_at.is_some()
                || patch.end_at.is_some()
                || patch.status.is_some())
        {
            return Err(SpanServiceError::Invalid(
                "title, time and status are managed by the provider",
            ));
        }
        match self.repo.update(actor.user_id, id, patch).await? {
            ConcurrencyOutcome::Success(span) => {
                self.notify(actor.user_id, "span_updated", span.id);
                self.user_events.sync_task_pin(
                    actor.user_id,
                    span.id,
                    serde_json::to_value(span.status)
                        .ok()
                        .as_ref()
                        .and_then(|v| v.as_str()),
                );
                Ok(span)
            }
            ConcurrencyOutcome::Conflict => Err(SpanServiceError::VersionConflict),
            ConcurrencyOutcome::NotFound => Err(SpanServiceError::NotFound),
        }
    }

    pub async fn get_span(&self, actor: &Actor, id: Uuid) -> Result<Option<Span>, sqlx::Error> {
        self.repo.get_by_id(actor.user_id, id).await
    }

    pub async fn list_spans(
        &self,
        actor: &Actor,
        query: &SpanQuery,
    ) -> Result<Vec<Span>, SpanServiceError> {
        if let (Some(from), Some(to)) = (query.from, query.to)
            && to < from
        {
            return Err(SpanServiceError::Invalid("to must not precede from"));
        }
        Ok(self.repo.list(actor.user_id, query).await?)
    }

    pub async fn day_counts(
        &self,
        actor: &Actor,
        query: &SpanDaysQuery,
    ) -> Result<SpanDays, SpanServiceError> {
        let tz = parse_timezone(&query.timezone)?;
        let span = (query.to_day - query.from_day).num_days();
        if !(0..MAX_DAYS_PER_REQUEST).contains(&span) {
            return Err(SpanServiceError::Invalid("day range must be 1 to 62 days"));
        }
        let bounds = (0..=span)
            .map(|offset| {
                let day = query.from_day + Duration::days(offset);
                day_bounds(day, tz).map(|(start, end)| (day, start, end))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(SpanDays {
            days: self.repo.day_counts(actor.user_id, &bounds).await?,
        })
    }

    pub async fn day_page(
        &self,
        actor: &Actor,
        query: &SpanDayQuery,
    ) -> Result<SpanDayPage, SpanServiceError> {
        let tz = parse_timezone(&query.timezone)?;
        let (start, end) = day_bounds(query.day, tz)?;
        let after = query.cursor.as_deref().map(decode_cursor).transpose()?;
        let limit = query.limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE);
        let mut items = self
            .repo
            .day_page(actor.user_id, start, end, after, limit + 1)
            .await?;
        let has_more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        let next_cursor = if has_more {
            items.last().and_then(encode_cursor)
        } else {
            None
        };
        Ok(SpanDayPage { items, next_cursor })
    }

    pub async fn delete_span(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        let deleted = self.repo.delete(actor.user_id, id).await?;
        if deleted {
            self.notify(actor.user_id, "span_deleted", id);
            self.user_events.sync_task_pin(actor.user_id, id, None);
        }
        Ok(deleted)
    }

    fn notify(&self, user_id: Uuid, kind: &str, span_id: Uuid) {
        self.user_events.notify(
            user_id,
            serde_json::json!({"type": kind, "span_id": span_id}),
        );
    }
}

fn missing_reference(err: sqlx::Error) -> SpanServiceError {
    match &err {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23503") => {
            SpanServiceError::ReferenceNotFound
        }
        _ => SpanServiceError::Database(err),
    }
}
