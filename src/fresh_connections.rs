use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use uuid::Uuid;
pub use vox_connections::accounts::*;
use vox_connections::providers::{
    google_calendar::GoogleCalendarEvent, observations::ObservedActivity,
};

struct CoreIngestor;
#[async_trait::async_trait]
impl TimelineIngestor for CoreIngestor {
    async fn calendar(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        connection_id: Uuid,
        account_id: &str,
        events: &[GoogleCalendarEvent],
        time_min: DateTime<Utc>,
        time_max: DateTime<Utc>,
    ) -> Result<usize, FreshConnectionError> {
        let now = Utc::now();
        let mut seen_ids = HashSet::new();
        let mut created_or_updated = 0;

        for event in events {
            let source_ref = format!("{account_id}:primary:{}", event.id);
            seen_ids.insert(source_ref.clone());
            let span_status = if event.status == "cancelled" {
                "cancelled"
            } else if event.end <= now {
                "done"
            } else if event.start <= now && event.end > now {
                "active"
            } else {
                "planned"
            };

            let payload = json!({
                "all_day": event.all_day,
                "time_zone": event.time_zone,
                "location": event.location,
                "recurring_event_id": event.recurring_event_id,
                "original_start": event.original_start,
                "start_date": event.start_date,
                "end_date": event.end_date,
                "connection_id": connection_id,
                "account_id": account_id,
            });

            let event_id: Uuid = sqlx::query_scalar(
                "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload, processed_at) \
                 VALUES ($1, 'google_calendar', $2, $3, $4, 'calendar.event_synced', $5, $6, now()) \
                 ON CONFLICT (source_kind, source_id, external_event_id) DO UPDATE SET occurred_at = EXCLUDED.occurred_at, payload = EXCLUDED.payload, payload_hash=EXCLUDED.payload_hash RETURNING id"
            )
            .bind(user_id)
            .bind(format!("{user_id}:{account_id}:primary"))
            .bind(&event.id)
            .bind(format!("{:x}", Sha256::digest(serde_json::to_vec(event).map_err(|_|FreshConnectionError::Invalid("Invalid calendar event".into()))?)))
            .bind(event.start)
            .bind(json!({"event":event,"connection_id":connection_id}))
            .fetch_one(&mut **tx)
            .await?;

            let existing_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM spans WHERE user_id = $1 AND source = 'google_calendar' AND source_ref = $2"
            )
            .bind(user_id)
            .bind(&source_ref)
            .fetch_optional(&mut **tx)
            .await?;

            if let Some(id) = existing_id {
                if event.status == "cancelled" && !event.has_time {
                    sqlx::query("UPDATE spans SET status='cancelled',version=version+1,updated_at=now() WHERE id=$1 AND status<>'cancelled'").bind(id).execute(&mut **tx).await?;
                    continue;
                }
                sqlx::query(
                    "UPDATE spans SET title = $1, start_at = $2, end_at = $3, status = $4, data = data || $5, source_event_id = $6, version = version + 1, updated_at = now() WHERE id = $7 AND (title, start_at, end_at, status, data || $5) IS DISTINCT FROM ($1, $2, $3, $4, data)"
                )
                .bind(&event.summary)
                .bind(event.start)
                .bind(event.end)
                .bind(span_status)
                .bind(&payload)
                .bind(event_id)
                .bind(id)
                .execute(&mut **tx)
                .await?;
            } else if event.status != "cancelled" {
                sqlx::query(
                    "INSERT INTO spans (user_id, title, notes, category, source, source_ref, status, start_at, end_at, data, source_event_id, version, created_at, updated_at) \
                     VALUES ($1, $2, '', 'calendar', 'google_calendar', $3, $4, $5, $6, $7, $8, 1, now(), now())"
                )
                .bind(user_id)
                .bind(&event.summary)
                .bind(&source_ref)
                .bind(span_status)
                .bind(event.start)
                .bind(event.end)
                .bind(&payload)
                .bind(event_id)
                .execute(&mut **tx)
                .await?;
                created_or_updated += 1;
            }
        }

        let existing_calendar_spans = sqlx::query(
            "SELECT id, source_ref FROM spans WHERE user_id = $1 AND source = 'google_calendar' AND start_at >= $2 AND start_at < $3 AND status <> 'cancelled' AND data->>'account_id'=$4"
        )
        .bind(user_id)
        .bind(time_min)
        .bind(time_max)
        .bind(account_id)
        .fetch_all(&mut **tx)
        .await?;

        for r in existing_calendar_spans {
            let id: Uuid = r.get("id");
            let source_ref: Option<String> = r.get("source_ref");
            if let Some(sref) = source_ref
                && !seen_ids.contains(&sref)
            {
                sqlx::query("UPDATE spans SET status = 'cancelled', version = version + 1, updated_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(&mut **tx)
                        .await?;
            }
        }

        notify(tx, user_id).await?;
        Ok(created_or_updated)
    }
    async fn gaming(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        connection_id: Uuid,
        activities: &[ObservedActivity],
    ) -> Result<usize, FreshConnectionError> {
        let mut spans_created = 0;

        {
            for act in activities {
                let payload = json!({
                    "game": act.game.name,
                    "platform": act.game.platform,
                    "duration_seconds": act.duration_seconds,
                    "title_id": act.game.title_id,
                    "observation_start": act.observation_start,
                    "observation_end": act.observation_end,
                    "timing": "observed_counter_delta",
                    "session_times_known": false,
                    "connection_id": connection_id,
                });

                let event_id: Uuid = sqlx::query_scalar(
                    "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload, processed_at) \
                     VALUES ($1, 'playstation', $2, $3, $4, 'gaming.playtime_observed', $5, $6, now()) \
                     ON CONFLICT (source_kind, source_id, external_event_id) DO UPDATE SET external_event_id = EXCLUDED.external_event_id RETURNING id"
                )
                .bind(user_id)
                .bind(connection_id.to_string())
                .bind(&act.source_ref)
                .bind(format!("{:x}", Sha256::digest(act.source_ref.as_bytes())))
                .bind(act.observation_start)
                .bind(&payload)
                .fetch_one(&mut **tx)
                .await?;

                let title = format!("PlayStation: {}", act.game.name);
                let notes = format!("Played on {}", act.game.platform);
                let hours = (act.duration_seconds as f64) / 3600.0;
                let minutes = ((act.duration_seconds % 3600) as f64) / 60.0;
                let desc = if hours >= 1.0 {
                    format!("{:.1}h", hours)
                } else {
                    format!("{:.0}m", minutes)
                };

                let span_data = json!({
                    "game": act.game.name,
                    "platform": act.game.platform,
                    "duration_seconds": act.duration_seconds,
                    "duration_display": desc,
                    "title_id": act.game.title_id,
                    "observation_start": act.observation_start,
                    "observation_end": act.observation_end,
                    "timing": "observed_counter_delta",
                    "session_times_known": false,
                    "connection_id": connection_id,
                });

                let existing = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM spans WHERE user_id = $1 AND source = 'playstation' AND source_ref = $2"
                )
                .bind(user_id)
                .bind(&act.source_ref)
                .fetch_optional(&mut **tx)
                .await?;

                if existing.is_none() {
                    sqlx::query(
                        "INSERT INTO spans (user_id, title, notes, category, source, source_ref, status, start_at, end_at, execution_type, data, source_event_id, version, created_at, updated_at) \
                         VALUES ($1, $2, $3, 'gaming', 'playstation', $4, 'done', $5, $6, 'manual_human', $7, $8, 1, now(), now())"
                    )
                    .bind(user_id)
                    .bind(title)
                    .bind(notes)
                    .bind(&act.source_ref)
                    .bind(Option::<DateTime<Utc>>::None)
                    .bind(Option::<DateTime<Utc>>::None)
                    .bind(span_data)
                    .bind(event_id)
                    .execute(&mut **tx)
                    .await?;
                    spans_created += 1;
                }
            }
        }

        notify(tx, user_id).await?;
        Ok(spans_created)
    }
}
async fn notify(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    user_id: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_notify('vox_connection_spans', $1)")
        .bind(json!({"type":"span_updated","user_id":user_id}).to_string())
        .execute(&mut **tx)
        .await?;
    Ok(())
}
#[derive(Clone)]
pub struct FreshConnectionsService(vox_connections::accounts::FreshConnectionsService);
impl std::ops::Deref for FreshConnectionsService {
    type Target = vox_connections::accounts::FreshConnectionsService;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl FreshConnectionsService {
    pub fn new(
        pool: PgPool,
        credential_key: Option<&str>,
        _hub: Option<crate::realtime::UserEventHub>,
        client: Option<String>,
        secret: Option<String>,
        core_url: Option<String>,
    ) -> Result<Self, FreshConnectionError> {
        Ok(Self(
            vox_connections::accounts::FreshConnectionsService::new(
                pool,
                credential_key,
                std::sync::Arc::new(CoreIngestor),
                client,
                secret,
                core_url,
            )?,
        ))
    }
}

#[cfg(test)]
mod configuration_tests {
    use super::*;
    fn service(key: Option<&str>) -> Result<FreshConnectionsService, FreshConnectionError> {
        FreshConnectionsService::new(
            sqlx::postgres::PgPoolOptions::new()
                .connect_lazy("postgres://localhost/unused")
                .unwrap(),
            key,
            None,
            Some("client".into()),
            Some("secret".into()),
            Some("https://core.example".into()),
        )
    }
    #[tokio::test]
    async fn missing_key_disables_every_connector() {
        assert!(
            service(None)
                .unwrap()
                .list_connectors()
                .iter()
                .all(|c| !c.available)
        );
    }
    #[tokio::test]
    async fn malformed_configured_key_is_rejected() {
        assert!(service(Some("invalid")).is_err());
    }
}

#[cfg(test)]
#[path = "connection_ingestion_tests.rs"]
mod ingestion_tests;
