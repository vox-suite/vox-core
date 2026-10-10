use crate::domain::pulse::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inventory {
    pub profiles: Vec<SourceProfile>,
    pub source_count: usize,
    pub record_count: i64,
}

#[derive(Clone, Debug)]
pub struct PulseMetadata {
    pub revision: String,
    pub connections: Vec<PulseConnection>,
    pub charts: Vec<SavedPulseChart>,
    pub dismissed: Vec<String>,
    pub saved_hashes: Vec<String>,
    pub caches: serde_json::Value,
    pub next_cursor: Option<Uuid>,
}

#[derive(Clone)]
pub struct PulseRepository {
    pub pool: PgPool,
}

impl PulseRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn metadata(
        &self,
        user: Uuid,
        cursor: Option<Uuid>,
    ) -> Result<PulseMetadata, sqlx::Error> {
        let rev_row = sqlx::query(
            "SELECT data_revision, discovery_revision FROM pulse_revisions WHERE user_id = $1",
        )
        .bind(user)
        .fetch_optional(&self.pool)
        .await?;

        let (data_rev, disc_rev) = rev_row
            .map(|r| {
                (
                    r.get::<i64, _>("data_revision"),
                    r.get::<i64, _>("discovery_revision"),
                )
            })
            .unwrap_or((0, 0));
        let revision = format!("{data_rev}:{disc_rev}");

        let conn_rows = sqlx::query(
            "SELECT connector_id, last_synced_at, authorization_state, sync_timeline, assistant_read \
             FROM vox_connections WHERE user_id = $1",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        let connections: Vec<PulseConnection> = conn_rows
            .into_iter()
            .map(|r| PulseConnection {
                connector_id: r.get("connector_id"),
                last_synced_at: r.get("last_synced_at"),
                authorization_state: r.get("authorization_state"),
                sync_timeline: r.get("sync_timeline"),
                assistant_read: r.get("assistant_read"),
            })
            .collect();

        let chart_rows = sqlx::query(
            "SELECT id, title, chart_type, definition, sort_order, is_pinned, created_at \
             FROM pulse_charts WHERE user_id = $1 AND ($2::uuid IS NULL OR id > $2) \
             ORDER BY id ASC LIMIT 13",
        )
        .bind(user)
        .bind(cursor)
        .fetch_all(&self.pool)
        .await?;

        let mut charts = Vec::with_capacity(chart_rows.len());
        for r in chart_rows {
            let def_val: serde_json::Value = r.get("definition");
            if let Ok(def) = serde_json::from_value::<PulseDefinition>(def_val) {
                charts.push(SavedPulseChart {
                    id: r.get("id"),
                    title: r.get("title"),
                    definition: def,
                    created_at: r.get("created_at"),
                    result: None,
                });
            }
        }

        let next_cursor = if charts.len() > 12 {
            charts.truncate(12);
            charts.last().map(|c| c.id)
        } else {
            None
        };

        let dismissed_rows = sqlx::query(
            "SELECT suggestion_key FROM pulse_dismissals WHERE user_id = $1 ORDER BY dismissed_at DESC",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        let dismissed: Vec<String> = dismissed_rows
            .into_iter()
            .map(|r| r.get("suggestion_key"))
            .collect();

        let cache_rows = sqlx::query(
            "SELECT cache_key, payload FROM pulse_cache WHERE user_id = $1 AND expires_at > now()",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;

        let mut caches = serde_json::Map::new();
        for r in cache_rows {
            caches.insert(r.get("cache_key"), r.get("payload"));
        }

        let saved_definitions: Vec<serde_json::Value> =
            sqlx::query_scalar("SELECT definition FROM pulse_charts WHERE user_id=$1")
                .bind(user)
                .fetch_all(&self.pool)
                .await?;
        let saved_hashes = saved_definitions
            .into_iter()
            .filter_map(|v| serde_json::from_value::<PulseDefinition>(v).ok())
            .map(|d| crate::application::pulse::measurements::definition_hash(&d))
            .collect();
        Ok(PulseMetadata {
            revision,
            connections,
            charts,
            dismissed: dismissed.clone(),
            saved_hashes,
            caches: serde_json::Value::Object(caches),
            next_cursor,
        })
    }

    pub async fn profiles(&self, user: Uuid) -> Result<Vec<SourceProfile>, sqlx::Error> {
        Ok(self.inventory(user).await?.profiles)
    }

    pub async fn inventory(&self, user: Uuid) -> Result<Inventory, sqlx::Error> {
        let rows = sqlx::query(r#"
            WITH base AS (
                SELECT te.*, et.value AS action, et.analytics_definition, g.value AS category,
                    coalesce(te.content->>'currency', '') AS currency,
                    coalesce(te.content->>'timing', '') AS timing
                FROM timeline_events te
                JOIN timeline_event_types et ON et.id = te.event_type_id
                JOIN timeline_groups g ON g.id = te.group_id
                WHERE te.user_id = $1 AND te.record_state = 'active' AND et.state = 'published'
                    AND te.occurred_at >= now() - interval '366 days'
            ), counts AS (
                SELECT event_type_id, category, action, currency, timing,
                    count(*)::bigint AS count, min(occurred_at) AS first_at, max(occurred_at) AS last_at,
                    count(*) FILTER (WHERE ended_at > occurred_at)::bigint AS known_intervals
                FROM base GROUP BY event_type_id, category, action, currency, timing
            ), fields AS (
                SELECT et.id AS event_type_id,jsonb_object_agg(property.key,
                    CASE WHEN property.value->>'type' IN ('number','integer') OR property.value->'type' @> '["number"]'::jsonb OR property.value->'type' @> '["integer"]'::jsonb THEN 'number'
                         WHEN property.value->>'type'='string' OR property.value->'type' @> '["string"]'::jsonb THEN 'string' ELSE 'object' END) AS fields
                FROM timeline_event_types et CROSS JOIN LATERAL jsonb_each(coalesce(et.content_schema->'properties','{}'::jsonb)) property
                WHERE et.id IN (SELECT event_type_id FROM counts) GROUP BY et.id
            )
            SELECT c.*, f.fields, et.analytics_definition
            FROM counts c JOIN timeline_event_types et ON et.id = c.event_type_id
            LEFT JOIN fields f USING (event_type_id)
            ORDER BY c.count DESC LIMIT 200
        "#).bind(user).fetch_all(&self.pool).await?;
        let mut profiles = Vec::with_capacity(rows.len());
        let mut total_records = 0;
        for r in rows {
            let id: Uuid = r.get("event_type_id");
            let cat: String = r.get("category");
            let act: String = r.get("action");
            let curr: String = r.get("currency");
            let timing: String = r.get("timing");
            let cnt: i64 = r.get("count");
            total_records += cnt;
            let fields: std::collections::BTreeMap<String, String> = serde_json::from_value(
                r.get::<Option<serde_json::Value>, _>("fields")
                    .unwrap_or(serde_json::json!({})),
            )
            .unwrap_or_default();
            profiles.push(SourceProfile {
                key: format!("{id}:{curr}:{timing}"),
                schema_id: Some(id),
                connection_id: None,
                source: act.clone(),
                category: cat,
                action: act,
                timing,
                currency: curr,
                count: cnt,
                dated_count: cnt,
                first_at: r.get("first_at"),
                last_at: r.get("last_at"),
                known_intervals: r.get("known_intervals"),
                fields,
                samples: vec![r.get("analytics_definition")],
            });
        }

        let source_count = profiles.len();
        Ok(Inventory {
            profiles,
            source_count,
            record_count: total_records,
        })
    }

    pub async fn list_charts(
        &self,
        user: Uuid,
        limit: i64,
        offset: i64,
    ) -> Result<Vec<SavedPulseChart>, sqlx::Error> {
        let safe_limit = limit.clamp(1, 50);
        let rows = sqlx::query(
            "SELECT id, title, chart_type, definition, created_at \
             FROM pulse_charts WHERE user_id = $1 \
             ORDER BY is_pinned DESC, sort_order ASC, created_at DESC \
             LIMIT $2 OFFSET $3",
        )
        .bind(user)
        .bind(safe_limit)
        .bind(offset.max(0))
        .fetch_all(&self.pool)
        .await?;

        let mut charts = Vec::with_capacity(rows.len());
        for r in rows {
            let def_val: serde_json::Value = r.get("definition");
            if let Ok(def) = serde_json::from_value::<PulseDefinition>(def_val) {
                charts.push(SavedPulseChart {
                    id: r.get("id"),
                    title: r.get("title"),
                    definition: def,
                    created_at: r.get("created_at"),
                    result: None,
                });
            }
        }
        Ok(charts)
    }

    pub async fn get_chart(
        &self,
        user: Uuid,
        id: Uuid,
    ) -> Result<Option<SavedPulseChart>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT id, title, chart_type, definition, created_at \
             FROM pulse_charts WHERE user_id = $1 AND id = $2",
        )
        .bind(user)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        if let Some(r) = row {
            let def_val: serde_json::Value = r.get("definition");
            if let Ok(def) = serde_json::from_value::<PulseDefinition>(def_val) {
                return Ok(Some(SavedPulseChart {
                    id: r.get("id"),
                    title: r.get("title"),
                    definition: def,
                    created_at: r.get("created_at"),
                    result: None,
                }));
            }
        }
        Ok(None)
    }

    pub async fn save(
        &self,
        user: Uuid,
        input: &SavePulseInput,
        _revision: &str,
        _hash: &str,
    ) -> Result<SavedPulseChart, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
            .bind(format!("pulse-charts:{user}"))
            .execute(&mut *tx)
            .await?;
        if let Some(existing) = sqlx::query("SELECT id, title, definition, created_at FROM pulse_charts WHERE user_id = $1 AND idempotency_key = $2")
            .bind(user).bind(input.idempotency_key).fetch_optional(&mut *tx).await? {
            return Ok(SavedPulseChart {
                id: existing.get("id"), title: existing.get("title"),
                definition: serde_json::from_value(existing.get("definition")).map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
                created_at: existing.get("created_at"), result: None,
            });
        }
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM pulse_charts WHERE user_id = $1")
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;

        if count >= 50 {
            return Err(sqlx::Error::Protocol(
                "Maximum limit of 50 saved charts reached".into(),
            ));
        }

        let chart_type = input.definition.chart_type.as_str();
        let def_json = serde_json::to_value(&input.definition)
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;

        let row = sqlx::query(
            "INSERT INTO pulse_charts (user_id, title, chart_type, definition, idempotency_key) \
             VALUES ($1, $2, $3, $4, $5) \
             RETURNING id, title, chart_type, definition, created_at",
        )
        .bind(user)
        .bind(input.title.trim())
        .bind(chart_type)
        .bind(def_json)
        .bind(input.idempotency_key)
        .fetch_one(&mut *tx)
        .await?;

        let chart = SavedPulseChart {
            id: row.get("id"),
            title: row.get("title"),
            definition: serde_json::from_value(row.get("definition"))
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            created_at: row.get("created_at"),
            result: None,
        };

        tx.commit().await?;
        Ok(chart)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_chart(
        &self,
        user: Uuid,
        id: Uuid,
        title: Option<&str>,
        chart_type: Option<&str>,
        definition: Option<&serde_json::Value>,
        sort_order: Option<i32>,
        is_pinned: Option<bool>,
    ) -> Result<Option<SavedPulseChart>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;

        let existing = sqlx::query(
            "SELECT id, title, chart_type, definition, sort_order, is_pinned, created_at \
             FROM pulse_charts WHERE user_id = $1 AND id = $2 FOR UPDATE",
        )
        .bind(user)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;

        let Some(row) = existing else {
            return Ok(None);
        };

        let new_title = title.unwrap_or_else(|| row.get("title"));
        let new_type = chart_type.unwrap_or_else(|| row.get("chart_type"));
        let existing_def: serde_json::Value = row.get("definition");
        let new_def = definition.cloned().unwrap_or(existing_def);
        let new_sort = sort_order.unwrap_or_else(|| row.get("sort_order"));
        let new_pin = is_pinned.unwrap_or_else(|| row.get("is_pinned"));

        let updated = sqlx::query(
            "UPDATE pulse_charts SET \
             title = $1, chart_type = $2, definition = $3, sort_order = $4, is_pinned = $5, updated_at = now() \
             WHERE user_id = $6 AND id = $7 \
             RETURNING id, title, chart_type, definition, created_at",
        )
        .bind(new_title)
        .bind(new_type)
        .bind(&new_def)
        .bind(new_sort)
        .bind(new_pin)
        .bind(user)
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        let def: PulseDefinition = serde_json::from_value(updated.get("definition"))
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;

        Ok(Some(SavedPulseChart {
            id: updated.get("id"),
            title: updated.get("title"),
            definition: def,
            created_at: updated.get("created_at"),
            result: None,
        }))
    }

    pub async fn delete_chart(&self, user: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let done = sqlx::query("DELETE FROM pulse_charts WHERE user_id = $1 AND id = $2")
            .bind(user)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }

    pub async fn dismiss(&self, user: Uuid, key: &str) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO pulse_dismissals (user_id, suggestion_key, dismissed_at) \
             VALUES ($1, $2, now()) \
             ON CONFLICT (user_id, suggestion_key) DO UPDATE SET dismissed_at = now()",
        )
        .bind(user)
        .bind(key)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn undismiss(&self, user: Uuid, key: &str) -> Result<bool, sqlx::Error> {
        let res =
            sqlx::query("DELETE FROM pulse_dismissals WHERE user_id = $1 AND suggestion_key = $2")
                .bind(user)
                .bind(key)
                .execute(&self.pool)
                .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_dismissals(&self, user: Uuid) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT suggestion_key FROM pulse_dismissals WHERE user_id = $1 ORDER BY dismissed_at DESC",
        )
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.into_iter().map(|r| r.get("suggestion_key")).collect())
    }

    pub async fn put_cache(
        &self,
        user: Uuid,
        key: &str,
        payload: serde_json::Value,
        seconds: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(r#"
            WITH pruned AS (
                DELETE FROM pulse_cache
                WHERE user_id = $1 AND cache_key IN (
                    SELECT cache_key FROM pulse_cache WHERE user_id = $1 ORDER BY expires_at DESC OFFSET 63
                )
            )
            INSERT INTO pulse_cache (user_id, cache_key, payload, expires_at, created_at)
            VALUES ($1, $2, $3, now() + ($4 || ' second')::interval, now())
            ON CONFLICT (user_id, cache_key) DO UPDATE SET
                payload = EXCLUDED.payload,
                expires_at = EXCLUDED.expires_at,
                created_at = now()
        "#)
        .bind(user)
        .bind(key)
        .bind(payload)
        .bind(seconds.to_string())
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn get_cache(
        &self,
        user: Uuid,
        key: &str,
    ) -> Result<Option<serde_json::Value>, sqlx::Error> {
        let row = sqlx::query_scalar(
            "SELECT payload FROM pulse_cache WHERE user_id = $1 AND cache_key = $2 AND expires_at > now()",
        )
        .bind(user)
        .bind(key)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row)
    }

    pub async fn invalidate(
        &self,
        user: Uuid,
        reason: &str,
        start: Option<DateTime<Utc>>,
        end: Option<DateTime<Utc>>,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) VALUES ($1, $2, $3, $4)",
        )
        .bind(user)
        .bind(reason)
        .bind(start)
        .bind(end)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn process_pending_pulse_invalidations(pool: &PgPool) -> Result<usize, sqlx::Error> {
        let mut tx = pool.begin().await?;
        let pending = sqlx::query("SELECT id, user_id FROM pulse_invalidations WHERE processed_at IS NULL ORDER BY created_at LIMIT 100 FOR UPDATE SKIP LOCKED")
            .fetch_all(&mut *tx).await?;
        let users: std::collections::BTreeSet<Uuid> =
            pending.iter().map(|r| r.get("user_id")).collect();
        for user in users {
            sqlx::query("DELETE FROM pulse_daily_aggregates WHERE user_id=$1 AND data_revision < coalesce((SELECT data_revision FROM pulse_revisions WHERE user_id=$1),0)")
                .bind(user).execute(&mut *tx).await?;
            sqlx::query("DELETE FROM pulse_cache WHERE user_id=$1 AND expires_at < now()")
                .bind(user)
                .execute(&mut *tx)
                .await?;
        }
        let ids: Vec<Uuid> = pending.iter().map(|r| r.get("id")).collect();
        sqlx::query("UPDATE pulse_invalidations SET processed_at=now() WHERE id=ANY($1)")
            .bind(&ids)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(ids.len())
    }
}
