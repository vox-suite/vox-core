use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use std::collections::HashMap;
use uuid::Uuid;

use crate::domain::timeline::{
    IngestTimelineEventInput, NewEventType, TimelineEvent, TimelineEventType,
    TimelineEventWithEvidence, TimelineEvidence, TimelineGroup, TimelinePage, TimelineQuery,
};

#[derive(Debug, thiserror::Error)]
pub enum TimelineStorageError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("event type not found")]
    EventTypeNotFound,
    #[error("timeline group not found")]
    GroupNotFound,
    #[error("group mismatch: event type group does not match requested group")]
    GroupMismatch,
    #[error("unauthorized event type ownership")]
    UnauthorizedEventType,
    #[error("invalid json schema: {0}")]
    InvalidJsonSchema(String),
    #[error("content schema validation failed: {0}")]
    ValidationFailed(String),
    #[error("invalid cursor")]
    InvalidCursor,
}

#[derive(Clone)]
pub struct TimelineRepository {
    pool: PgPool,
}

impl TimelineRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn list_groups(&self) -> Result<Vec<TimelineGroup>, TimelineStorageError> {
        let rows = sqlx::query(
            "SELECT id, value, label, ui_hint, sort_order FROM timeline_groups ORDER BY sort_order ASC",
        )
        .fetch_all(&self.pool)
        .await?;

        let mut groups = Vec::with_capacity(rows.len());
        for row in rows {
            groups.push(TimelineGroup {
                id: row.get("id"),
                value: row.get("value"),
                label: row.get("label"),
                ui_hint: row.get("ui_hint"),
                sort_order: row.get("sort_order"),
            });
        }
        Ok(groups)
    }

    pub async fn list_event_types(
        &self,
        user_id: Uuid,
        group_id: Option<Uuid>,
        group_value: Option<&str>,
    ) -> Result<Vec<TimelineEventType>, TimelineStorageError> {
        let rows = sqlx::query(
            "SELECT et.id, et.owner_user_id, et.value, et.version, et.label, et.group_id, \
                    et.description, et.content_schema, et.analytics_definition, et.ui_hint, \
                    et.state, et.created_at \
             FROM timeline_event_types et \
             JOIN timeline_groups g ON g.id = et.group_id \
             WHERE et.state='published' AND (et.owner_user_id IS NULL OR et.owner_user_id = $1) \
               AND ($2::uuid IS NULL OR et.group_id = $2) \
               AND ($3::text IS NULL OR g.value = $3) \
             ORDER BY et.created_at ASC",
        )
        .bind(user_id)
        .bind(group_id)
        .bind(group_value)
        .fetch_all(&self.pool)
        .await?;

        let mut types = Vec::with_capacity(rows.len());
        for row in rows {
            types.push(TimelineEventType {
                id: row.get("id"),
                owner_user_id: row.get("owner_user_id"),
                value: row.get("value"),
                version: row.get("version"),
                label: row.get("label"),
                group_id: row.get("group_id"),
                description: row.get("description"),
                content_schema: row.get("content_schema"),
                analytics_definition: row.get("analytics_definition"),
                ui_hint: row.get("ui_hint"),
                state: row.get("state"),
                created_at: row.get("created_at"),
            });
        }
        Ok(types)
    }

    pub async fn create_event_type(
        &self,
        user_id: Uuid,
        input: NewEventType,
    ) -> Result<TimelineEventType, TimelineStorageError> {
        validate_schema_document(&input.content_schema)?;
        if !input.analytics_definition.is_object()
            || !input.ui_hint.is_object()
            || serde_json::to_vec(&input.analytics_definition)
                .map_or(true, |bytes| bytes.len() > 65536)
            || serde_json::to_vec(&input.ui_hint).map_or(true, |bytes| bytes.len() > 16384)
            || input.description.len() > 4000
        {
            return Err(TimelineStorageError::ValidationFailed(
                "invalid analytics or UI metadata".into(),
            ));
        }
        if input.value.is_empty()
            || input.value.len() > 80
            || !input
                .value
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
            || input.label.trim().is_empty()
            || input.label.len() > 120
        {
            return Err(TimelineStorageError::ValidationFailed(
                "invalid event type value or label".into(),
            ));
        }
        if let Err(err) = jsonschema::validator_for(&input.content_schema) {
            return Err(TimelineStorageError::InvalidJsonSchema(err.to_string()));
        }

        let group_exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM timeline_groups WHERE id = $1)",
        )
        .bind(input.group_id)
        .fetch_one(&self.pool)
        .await?;

        if !group_exists {
            return Err(TimelineStorageError::GroupNotFound);
        }

        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(format!("event-type:{user_id}:{}", input.value))
            .execute(&mut *tx)
            .await?;
        let next_version = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM timeline_event_types WHERE owner_user_id = $1 AND value = $2",
        )
        .bind(user_id)
        .bind(&input.value)
        .fetch_one(&mut *tx)
        .await?;

        let row = sqlx::query(
            "INSERT INTO timeline_event_types (owner_user_id, value, version, label, group_id, description, content_schema, analytics_definition, ui_hint, state) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'published') \
             RETURNING id, owner_user_id, value, version, label, group_id, description, content_schema, analytics_definition, ui_hint, state, created_at",
        )
        .bind(user_id)
        .bind(&input.value)
        .bind(next_version)
        .bind(&input.label)
        .bind(input.group_id)
        .bind(&input.description)
        .bind(&input.content_schema)
        .bind(&input.analytics_definition)
        .bind(&input.ui_hint)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(TimelineEventType {
            id: row.get("id"),
            owner_user_id: row.get("owner_user_id"),
            value: row.get("value"),
            version: row.get("version"),
            label: row.get("label"),
            group_id: row.get("group_id"),
            description: row.get("description"),
            content_schema: row.get("content_schema"),
            analytics_definition: row.get("analytics_definition"),
            ui_hint: row.get("ui_hint"),
            state: row.get("state"),
            created_at: row.get("created_at"),
        })
    }

    pub async fn day_counts(
        &self,
        user_id: Uuid,
        input: crate::domain::timeline::TimelineCountsQuery,
    ) -> Result<Vec<crate::domain::timeline::TimelineDayCount>, TimelineStorageError> {
        if input.timezone.parse::<chrono_tz::Tz>().is_err()
            || input.end_at <= input.start_at
            || (input.end_at - input.start_at).num_days() > 366
        {
            return Err(TimelineStorageError::ValidationFailed(
                "invalid timezone or date range; maximum 366 days".into(),
            ));
        }
        let rows = sqlx::query("SELECT (e.occurred_at AT TIME ZONE $4)::date::text AS day,g.value AS category,count(*)::bigint AS count FROM timeline_events e JOIN timeline_groups g ON g.id=e.group_id WHERE e.user_id=$1 AND e.record_state='active' AND e.occurred_at >= $2 AND e.occurred_at < $3 AND ($5::text IS NULL OR g.value=$5) GROUP BY 1,2 ORDER BY 1")
            .bind(user_id).bind(input.start_at).bind(input.end_at).bind(input.timezone).bind(input.group_value).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|row| crate::domain::timeline::TimelineDayCount {
                day: row.get("day"),
                category: row.get("category"),
                count: row.get("count"),
            })
            .collect())
    }

    pub async fn query_events(
        &self,
        user_id: Uuid,
        query: TimelineQuery,
    ) -> Result<TimelinePage, TimelineStorageError> {
        let (cursor_time, cursor_id) = match query.cursor.as_deref() {
            Some(cursor_str) => {
                let decoded = URL_SAFE_NO_PAD
                    .decode(cursor_str)
                    .map_err(|_| TimelineStorageError::InvalidCursor)?;
                let text =
                    String::from_utf8(decoded).map_err(|_| TimelineStorageError::InvalidCursor)?;
                let parts: Vec<&str> = text.split('|').collect();
                if parts.len() != 2 {
                    return Err(TimelineStorageError::InvalidCursor);
                }
                let dt = DateTime::parse_from_rfc3339(parts[0])
                    .map_err(|_| TimelineStorageError::InvalidCursor)?
                    .with_timezone(&Utc);
                let id =
                    Uuid::parse_str(parts[1]).map_err(|_| TimelineStorageError::InvalidCursor)?;
                (Some(dt), Some(id))
            }
            None => (None, None),
        };

        let fetch_limit = query.limit.unwrap_or(50).clamp(1, 100);

        let rows = sqlx::query(
            "SELECT e.id, e.user_id, e.event_type_id, e.group_id, e.title, e.summary, \
                    e.occurred_at, e.ended_at, e.time_precision, e.source_timezone, \
                    e.content, e.record_state, e.confidence::float8 AS confidence, \
                    e.dedupe_key, e.revision, e.created_at, e.updated_at \
             FROM timeline_events e \
             JOIN timeline_groups g ON g.id = e.group_id \
             JOIN timeline_event_types et ON et.id = e.event_type_id \
             WHERE e.user_id = $1 \
               AND ($2::uuid IS NULL OR e.group_id = $2) \
               AND ($3::text IS NULL OR g.value = $3) \
               AND ($4::uuid IS NULL OR e.event_type_id = $4) \
               AND ($5::text IS NULL OR et.value = $5) \
               AND ($6::timestamptz IS NULL OR e.occurred_at >= $6) \
               AND ($7::timestamptz IS NULL OR e.occurred_at < $7) \
               AND ($8::text IS NULL OR e.record_state = $8) \
               AND ($9::timestamptz IS NULL OR (e.occurred_at, e.id) < ($9, $10)) \
             ORDER BY e.occurred_at DESC, e.id DESC \
             LIMIT $11",
        )
        .bind(user_id)
        .bind(query.group_id)
        .bind(query.group_value.as_deref())
        .bind(query.event_type_id)
        .bind(query.event_type_value.as_deref())
        .bind(query.start_at)
        .bind(query.end_at)
        .bind(query.record_state.as_deref().unwrap_or("active"))
        .bind(cursor_time)
        .bind(cursor_id)
        .bind(fetch_limit + 1)
        .fetch_all(&self.pool)
        .await?;

        let has_more = rows.len() > fetch_limit as usize;
        let result_rows = if has_more {
            &rows[..fetch_limit as usize]
        } else {
            &rows[..]
        };

        let mut events = Vec::with_capacity(result_rows.len());
        let mut event_ids = Vec::with_capacity(result_rows.len());

        for row in result_rows {
            let event = TimelineEvent {
                id: row.get("id"),
                user_id: row.get("user_id"),
                event_type_id: row.get("event_type_id"),
                group_id: row.get("group_id"),
                title: row.get("title"),
                summary: row.get("summary"),
                occurred_at: row.get("occurred_at"),
                ended_at: row.get("ended_at"),
                time_precision: row.get("time_precision"),
                source_timezone: row.get("source_timezone"),
                content: row.get("content"),
                record_state: row.get("record_state"),
                confidence: row.get("confidence"),
                dedupe_key: row.get("dedupe_key"),
                revision: row.get("revision"),
                created_at: row.get("created_at"),
                updated_at: row.get("updated_at"),
            };
            event_ids.push(event.id);
            events.push(event);
        }

        let mut evidence_map: HashMap<Uuid, Vec<TimelineEvidence>> = HashMap::new();
        if !event_ids.is_empty() {
            let evidence_rows = sqlx::query(
                "SELECT id, timeline_event_id, user_id, source_record_id, source_attachment_id, \
                        source_type, source_id, raw_reference, observation_metadata, created_at \
                 FROM timeline_evidence \
                 WHERE user_id = $1 AND timeline_event_id = ANY($2) \
                 ORDER BY created_at ASC",
            )
            .bind(user_id)
            .bind(&event_ids)
            .fetch_all(&self.pool)
            .await?;

            for row in evidence_rows {
                let event_id: Uuid = row.get("timeline_event_id");
                let item = TimelineEvidence {
                    id: row.get("id"),
                    timeline_event_id: event_id,
                    user_id: row.get("user_id"),
                    source_record_id: row.get("source_record_id"),
                    source_attachment_id: row.get("source_attachment_id"),
                    source_type: row.get("source_type"),
                    source_id: row.get("source_id"),
                    raw_reference: row.get("raw_reference"),
                    observation_metadata: row.get("observation_metadata"),
                    created_at: row.get("created_at"),
                };
                evidence_map.entry(event_id).or_default().push(item);
            }
        }

        let next_cursor = if has_more {
            events.last().map(|e| {
                let token = format!("{}|{}", e.occurred_at.to_rfc3339(), e.id);
                URL_SAFE_NO_PAD.encode(token.as_bytes())
            })
        } else {
            None
        };

        let items: Vec<TimelineEventWithEvidence> = events
            .into_iter()
            .map(|e| {
                let evid = evidence_map.remove(&e.id).unwrap_or_default();
                TimelineEventWithEvidence {
                    event: e,
                    evidence: evid,
                }
            })
            .collect();

        Ok(TimelinePage {
            events: items,
            next_cursor,
        })
    }

    pub async fn ingest_event(
        &self,
        user_id: Uuid,
        input: IngestTimelineEventInput,
    ) -> Result<TimelineEventWithEvidence, TimelineStorageError> {
        let mut tx = self.pool.begin().await?;
        let mut result = self
            .ingest_event_in_transaction(&mut tx, user_id, input)
            .await?;
        let finance: bool = sqlx::query_scalar("SELECT owner_user_id IS NULL AND value IN ('transaction','bill','statement','refund','transfer','repayment') FROM timeline_event_types WHERE id=$1")
            .bind(result.event.event_type_id).fetch_one(&mut *tx).await?;
        if finance {
            crate::finance_normalization::dedupe_or_settle_in_transaction(
                &mut tx,
                user_id,
                result.event.id,
                &result.event.content,
            )
            .await?;
            let row = sqlx::query("SELECT content,record_state,revision,updated_at FROM timeline_events WHERE id=$1 AND user_id=$2")
                .bind(result.event.id).bind(user_id).fetch_one(&mut *tx).await?;
            result.event.content = row.get("content");
            result.event.record_state = row.get("record_state");
            result.event.revision = row.get("revision");
            result.event.updated_at = row.get("updated_at");
            if result.event.record_state != "active" {
                result.evidence.clear();
            }
        }
        tx.commit().await?;
        Ok(result)
    }
    pub async fn ingest_event_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        input: IngestTimelineEventInput,
    ) -> Result<TimelineEventWithEvidence, TimelineStorageError> {
        if input.title.trim().is_empty()
            || input.title.len() > 500
            || !input.content.is_object()
            || serde_json::to_vec(&input.content).map_or(true, |v| v.len() > 262_144)
            || input.evidence.len() > 20
            || input.evidence.iter().any(|item| {
                item.source_type.is_empty()
                    || item.source_type.len() > 100
                    || !item.observation_metadata.is_object()
                    || serde_json::to_vec(&item.observation_metadata)
                        .map_or(true, |bytes| bytes.len() > 65536)
                    || item
                        .source_id
                        .as_ref()
                        .is_some_and(|value| value.len() > 1024)
                    || item
                        .raw_reference
                        .as_ref()
                        .is_some_and(|value| value.len() > 2048)
            })
            || !input.confidence.is_finite()
            || !(0.0..=1.0).contains(&input.confidence)
            || input.ended_at.is_some_and(|end| end < input.occurred_at)
        {
            return Err(TimelineStorageError::ValidationFailed(
                "invalid event title, content, confidence or time range".into(),
            ));
        }

        if !matches!(
            input.time_precision.as_str(),
            "year" | "month" | "day" | "hour" | "minute" | "second" | "millisecond"
        ) || input
            .source_timezone
            .as_ref()
            .is_some_and(|timezone| timezone.parse::<chrono_tz::Tz>().is_err())
            || input
                .dedupe_key
                .as_ref()
                .is_some_and(|key| key.is_empty() || key.len() > 1024)
            || input
                .summary
                .as_ref()
                .is_some_and(|summary| summary.len() > 8000)
        {
            return Err(TimelineStorageError::ValidationFailed(
                "invalid precision, timezone, dedupe key or summary".into(),
            ));
        }
        let event_type_row = if let Some(et_id) = input.event_type_id {
            sqlx::query(
                "SELECT id, owner_user_id, group_id, content_schema \
                 FROM timeline_event_types WHERE id = $1 AND state = 'published'",
            )
            .bind(et_id)
            .fetch_optional(&mut **tx)
            .await?
        } else if let Some(ref val) = input.event_type_value {
            sqlx::query(
                "SELECT id, owner_user_id, group_id, content_schema \
                 FROM timeline_event_types \
                 WHERE state = 'published' AND value = $1 AND (owner_user_id = $2 OR owner_user_id IS NULL) \
                 ORDER BY owner_user_id NULLS LAST, version DESC \
                 LIMIT 1",
            )
            .bind(val)
            .bind(user_id)
            .fetch_optional(&mut **tx)
            .await?
        } else {
            None
        };

        let event_type = event_type_row.ok_or(TimelineStorageError::EventTypeNotFound)?;
        let et_id: Uuid = event_type.get("id");
        let owner: Option<Uuid> = event_type.get("owner_user_id");
        let et_group_id: Uuid = event_type.get("group_id");
        let content_schema: serde_json::Value = event_type.get("content_schema");

        if let Some(owner_id) = owner
            && owner_id != user_id
        {
            return Err(TimelineStorageError::UnauthorizedEventType);
        }

        let target_group_id = if let Some(gid) = input.group_id {
            gid
        } else if let Some(ref gval) = input.group_value {
            let gid =
                sqlx::query_scalar::<_, Uuid>("SELECT id FROM timeline_groups WHERE value = $1")
                    .bind(gval)
                    .fetch_optional(&mut **tx)
                    .await?
                    .ok_or(TimelineStorageError::GroupNotFound)?;
            gid
        } else {
            et_group_id
        };

        if target_group_id != et_group_id {
            return Err(TimelineStorageError::GroupMismatch);
        }

        validate_schema_document(&content_schema)?;
        let validator = jsonschema::validator_for(&content_schema)
            .map_err(|e| TimelineStorageError::InvalidJsonSchema(e.to_string()))?;

        if !validator.is_valid(&input.content) {
            let mut errs = Vec::new();
            for err in validator.iter_errors(&input.content) {
                errs.push(err.to_string());
            }
            return Err(TimelineStorageError::ValidationFailed(errs.join("; ")));
        }

        let event_row = if let Some(ref dkey) = input.dedupe_key {
            sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, \
                                             occurred_at, ended_at, time_precision, source_timezone, \
                                             content, record_state, confidence, dedupe_key) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'active', $11, $12) \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                     event_type_id = EXCLUDED.event_type_id, \
                     group_id = EXCLUDED.group_id, \
                     title = EXCLUDED.title, \
                     summary = EXCLUDED.summary, \
                     occurred_at = EXCLUDED.occurred_at, \
                     ended_at = EXCLUDED.ended_at, \
                     time_precision = EXCLUDED.time_precision, \
                     source_timezone = EXCLUDED.source_timezone, \
                     content = EXCLUDED.content, \
                     record_state = 'active', \
                     confidence = EXCLUDED.confidence, \
                     revision = timeline_events.revision + 1, \
                     updated_at = now() \
                 RETURNING id, user_id, event_type_id, group_id, title, summary, \
                           occurred_at, ended_at, time_precision, source_timezone, \
                           content, record_state, confidence::float8 AS confidence, \
                           dedupe_key, revision, created_at, updated_at",
            )
            .bind(user_id)
            .bind(et_id)
            .bind(target_group_id)
            .bind(&input.title)
            .bind(&input.summary)
            .bind(input.occurred_at)
            .bind(input.ended_at)
            .bind(&input.time_precision)
            .bind(&input.source_timezone)
            .bind(&input.content)
            .bind(input.confidence)
            .bind(dkey)
            .fetch_one(&mut **tx)
            .await?
        } else {
            sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, \
                                             occurred_at, ended_at, time_precision, source_timezone, \
                                             content, record_state, confidence, dedupe_key) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, 'active', $11, NULL) \
                 RETURNING id, user_id, event_type_id, group_id, title, summary, \
                           occurred_at, ended_at, time_precision, source_timezone, \
                           content, record_state, confidence::float8 AS confidence, \
                           dedupe_key, revision, created_at, updated_at",
            )
            .bind(user_id)
            .bind(et_id)
            .bind(target_group_id)
            .bind(&input.title)
            .bind(&input.summary)
            .bind(input.occurred_at)
            .bind(input.ended_at)
            .bind(&input.time_precision)
            .bind(&input.source_timezone)
            .bind(&input.content)
            .bind(input.confidence)
            .fetch_one(&mut **tx)
            .await?
        };

        let event = TimelineEvent {
            id: event_row.get("id"),
            user_id: event_row.get("user_id"),
            event_type_id: event_row.get("event_type_id"),
            group_id: event_row.get("group_id"),
            title: event_row.get("title"),
            summary: event_row.get("summary"),
            occurred_at: event_row.get("occurred_at"),
            ended_at: event_row.get("ended_at"),
            time_precision: event_row.get("time_precision"),
            source_timezone: event_row.get("source_timezone"),
            content: event_row.get("content"),
            record_state: event_row.get("record_state"),
            confidence: event_row.get("confidence"),
            dedupe_key: event_row.get("dedupe_key"),
            revision: event_row.get("revision"),
            created_at: event_row.get("created_at"),
            updated_at: event_row.get("updated_at"),
        };

        let mut inserted_evidence = Vec::with_capacity(input.evidence.len());
        for ev in input.evidence {
            let ev_row = sqlx::query(
                "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_record_id, \
                                               source_attachment_id, source_type, source_id, \
                                               raw_reference, observation_metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
                 ON CONFLICT (timeline_event_id,user_id,evidence_hash) DO UPDATE SET evidence_hash=EXCLUDED.evidence_hash \
                 RETURNING id, timeline_event_id, user_id, source_record_id, source_attachment_id, \
                           source_type, source_id, raw_reference, observation_metadata, created_at",
            )
            .bind(event.id)
            .bind(user_id)
            .bind(ev.source_record_id)
            .bind(ev.source_attachment_id)
            .bind(&ev.source_type)
            .bind(&ev.source_id)
            .bind(&ev.raw_reference)
            .bind(&ev.observation_metadata)
            .fetch_one(&mut **tx)
            .await?;

            inserted_evidence.push(TimelineEvidence {
                id: ev_row.get("id"),
                timeline_event_id: ev_row.get("timeline_event_id"),
                user_id: ev_row.get("user_id"),
                source_record_id: ev_row.get("source_record_id"),
                source_attachment_id: ev_row.get("source_attachment_id"),
                source_type: ev_row.get("source_type"),
                source_id: ev_row.get("source_id"),
                raw_reference: ev_row.get("raw_reference"),
                observation_metadata: ev_row.get("observation_metadata"),
                created_at: ev_row.get("created_at"),
            });
        }

        sqlx::query(
            "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
             VALUES ($1, 'timeline_ingest', $2, $2)",
        )
        .bind(user_id)
        .bind(event.occurred_at)
        .execute(&mut **tx)
        .await?;

        Ok(TimelineEventWithEvidence {
            event,
            evidence: inserted_evidence,
        })
    }
}

fn validate_schema_document(value: &serde_json::Value) -> Result<(), TimelineStorageError> {
    if serde_json::to_vec(value).map_or(true, |v| v.len() > 65_536) {
        return Err(TimelineStorageError::InvalidJsonSchema(
            "schema exceeds 64 KiB".into(),
        ));
    }
    fn inspect(value: &serde_json::Value, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        match value {
            serde_json::Value::Object(map) => map.iter().all(|(key, v)| {
                if key == "$ref" || key == "$dynamicRef" || key == "$recursiveRef" {
                    v.as_str().is_some_and(|r| r.starts_with('#'))
                } else if key == "$id" {
                    false
                } else {
                    inspect(v, depth + 1)
                }
            }),
            serde_json::Value::Array(items) => items.iter().all(|v| inspect(v, depth + 1)),
            _ => true,
        }
    }
    if !inspect(value, 0) {
        return Err(TimelineStorageError::InvalidJsonSchema(
            "only local references and depth <= 32 are supported".into(),
        ));
    }
    Ok(())
}

pub async fn validate_published_content(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    value: &str,
    content: &serde_json::Value,
) -> Result<(), TimelineStorageError> {
    let schema: serde_json::Value = sqlx::query_scalar("SELECT content_schema FROM timeline_event_types WHERE value=$1 AND owner_user_id IS NULL AND state='published' ORDER BY version DESC LIMIT 1")
        .bind(value).fetch_optional(&mut **tx).await?.ok_or(TimelineStorageError::EventTypeNotFound)?;
    validate_schema_document(&schema)?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| TimelineStorageError::InvalidJsonSchema(error.to_string()))?;
    if !validator.is_valid(content) {
        return Err(TimelineStorageError::ValidationFailed(
            "content does not match published event type".into(),
        ));
    }
    Ok(())
}
