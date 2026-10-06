use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::{HashMap, HashSet};

const MAX_ESTIMATED_SESSIONS: i64 = 60;
use uuid::Uuid;
pub use vox_connections::accounts::*;
use vox_connections::providers::{
    food_delivery::{FoodDeliveryOrder, FoodDeliveryStatus},
    google_calendar::GoogleCalendarEvent,
    observations::ObservedActivity,
    personal::PersonalActivity,
    playstation::PlayStationGame,
};

struct CoreIngestor {
    hub: Option<crate::realtime::UserEventHub>,
}
#[async_trait::async_trait]
impl TimelineIngestor for CoreIngestor {
    async fn personal_activity(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        connection_id: Uuid,
        connector: &str,
        items: &[PersonalActivity],
    ) -> Result<usize, FreshConnectionError> {
        let connector = match connector {
            "youtube_history" => "youtube",
            "maps_timeline" => "google_maps",
            other => other,
        };
        let category = match connector {
            "spotify" => "music",
            "youtube" => "video",
            "google_maps" => "visit",
            _ => {
                return Err(FreshConnectionError::Invalid(
                    "Unsupported activity source".into(),
                ));
            }
        };
        struct Row {
            source_id: String,
            hash: String,
            event_type: &'static str,
            occurred_at: DateTime<Utc>,
            event_payload: serde_json::Value,
            title: String,
            source_ref: String,
            ended_at: Option<DateTime<Utc>>,
            payload: serde_json::Value,
        }
        // Later duplicates of the same record win, as with row-by-row upserts.
        let mut rows: Vec<Row> = Vec::with_capacity(items.len());
        let mut position: HashMap<String, usize> = HashMap::new();
        for item in items {
            let (event_type, title) = match (connector, item.provider_data["action"].as_str()) {
                ("spotify", _) => ("music.listened", item.title.clone()),
                ("google_maps", Some("visit")) => ("place.visited", item.title.clone()),
                ("youtube", Some("watch")) if item.provider_data["watch_event"] == true => {
                    ("video.watched", item.title.clone())
                }
                ("youtube", Some("like")) => {
                    ("video.liked", format!("YouTube like: {}", item.title))
                }
                ("youtube", Some("playlist_addition")) => (
                    "video.playlist_added",
                    format!("YouTube playlist addition: {}", item.title),
                ),
                _ => {
                    return Err(FreshConnectionError::Invalid(
                        "Unsupported personal activity".into(),
                    ));
                }
            };
            if item.source_id.trim().is_empty()
                || item.title.trim().is_empty()
                || item.ended_at.is_some_and(|end| end < item.occurred_at)
            {
                return Err(FreshConnectionError::Invalid(
                    "Invalid personal activity".into(),
                ));
            }
            let payload = json!({
                "connection_id": connection_id,
                "source_id": item.source_id,
                "provider_data": item.provider_data,
                "timing": "provider_timestamp",
                "session_times_known": item.ended_at.is_some(),
            });
            let event_payload = json!({
                "title": item.title, "occurred_at": item.occurred_at,
                "ended_at": item.ended_at, "activity": payload,
            });
            let row = Row {
                source_id: item.source_id.clone(),
                hash: format!(
                    "{:x}",
                    Sha256::digest(serde_json::to_vec(&event_payload).map_err(|_| {
                        FreshConnectionError::Invalid("Invalid activity payload".into())
                    })?)
                ),
                event_type,
                occurred_at: item.occurred_at,
                event_payload,
                title,
                source_ref: format!("{connector}:{}", item.source_id),
                ended_at: item.ended_at,
                payload,
            };
            match position.get(&row.source_id) {
                Some(&index) => rows[index] = row,
                None => {
                    position.insert(row.source_id.clone(), rows.len());
                    rows.push(row);
                }
            }
        }
        let mut created = 0;
        // Batched upserts: a large import is a few statements, not two per record.
        for chunk in rows.chunks(500) {
            let events: Vec<(String, Uuid)> = sqlx::query_as(
                "INSERT INTO inbound_events (user_id,source_kind,source_id,external_event_id,payload_hash,event_type,occurred_at,payload,processed_at) \
                 SELECT $1,$2,$3,u.ext,u.hash,u.etype,u.occ,u.payload,now() \
                 FROM UNNEST($4::text[],$5::text[],$6::text[],$7::timestamptz[],$8::jsonb[]) AS u(ext,hash,etype,occ,payload) \
                 ON CONFLICT (source_kind,source_id,external_event_id) DO UPDATE SET payload=EXCLUDED.payload,payload_hash=EXCLUDED.payload_hash,occurred_at=EXCLUDED.occurred_at \
                 RETURNING external_event_id,id"
            ).bind(user_id).bind(connector).bind(format!("{user_id}:{connector}"))
                .bind(chunk.iter().map(|r| r.source_id.clone()).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.hash.clone()).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.event_type.to_string()).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.occurred_at).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.event_payload.clone()).collect::<Vec<_>>())
                .fetch_all(&mut **tx).await?;
            let event_ids: HashMap<String, Uuid> = events.into_iter().collect();
            let mut event_for_row = Vec::with_capacity(chunk.len());
            for row in chunk {
                event_for_row.push(*event_ids.get(&row.source_id).ok_or_else(|| {
                    FreshConnectionError::Invalid("Invalid activity payload".into())
                })?);
            }
            let inserted: Vec<bool> = sqlx::query_scalar(
                "INSERT INTO spans (user_id,title,category,source,source_ref,status,start_at,end_at,execution_type,data,source_event_id) \
                 SELECT $1,u.title,$2,$3,u.source_ref,'done',u.start_at,u.end_at,'manual_human',u.data,u.event_id \
                 FROM UNNEST($4::text[],$5::text[],$6::timestamptz[],$7::timestamptz[],$8::jsonb[],$9::uuid[]) AS u(title,source_ref,start_at,end_at,data,event_id) \
                 ON CONFLICT (user_id,source,source_ref) DO UPDATE SET \
                 title=EXCLUDED.title,start_at=EXCLUDED.start_at,end_at=EXCLUDED.end_at, \
                 data=spans.data || EXCLUDED.data,source_event_id=EXCLUDED.source_event_id,version=spans.version+1,updated_at=now() \
                 WHERE (spans.title,spans.start_at,spans.end_at,spans.data || EXCLUDED.data,spans.source_event_id) \
                 IS DISTINCT FROM (EXCLUDED.title,EXCLUDED.start_at,EXCLUDED.end_at,spans.data,EXCLUDED.source_event_id) RETURNING (xmax=0)"
            ).bind(user_id).bind(category).bind(connector)
                .bind(chunk.iter().map(|r| r.title.clone()).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.source_ref.clone()).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.occurred_at).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.ended_at).collect::<Vec<_>>())
                .bind(chunk.iter().map(|r| r.payload.clone()).collect::<Vec<_>>())
                .bind(event_for_row)
                .fetch_all(&mut **tx).await?;
            created += inserted.into_iter().filter(|new| *new).count();
        }
        notify(tx, user_id).await?;
        Ok(created)
    }
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
                 ON CONFLICT (source_kind, source_id, external_event_id) DO UPDATE SET occurred_at = EXCLUDED.occurred_at, payload = EXCLUDED.payload RETURNING id"
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

                let title = act.game.name.clone();
                let notes = format!("Played on {}", act.game.platform);
                let hours = (act.duration_seconds as f64) / 3600.0;
                let minutes = ((act.duration_seconds % 3600) as f64) / 60.0;
                let desc = if hours >= 1.0 {
                    format!("{:.1}h", hours)
                } else {
                    format!("{:.0}m", minutes)
                };

                // PlayStation reports when the last session ended; if that falls inside
                // the sync window, the new play time ended then.
                let session_end = act.game.last_played_at.filter(|last| {
                    *last >= act.observation_start - chrono::Duration::minutes(30)
                        && *last <= act.observation_end + chrono::Duration::minutes(5)
                });
                let session_start = session_end.map(|end| {
                    (end - chrono::Duration::seconds(act.duration_seconds as i64))
                        .max(act.observation_start - chrono::Duration::minutes(30))
                });
                let span_data = json!({
                    "image_url": act.game.image_url,
                    "session_estimated": session_end.is_some(),
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
                    .bind(session_start)
                    .bind(session_end)
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

    async fn game_history(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        connection_id: Uuid,
        games: &[PlayStationGame],
    ) -> Result<usize, FreshConnectionError> {
        sqlx::query(
            "DELETE FROM spans WHERE user_id = $1 AND source = 'playstation' AND source_ref LIKE 'history:%'",
        )
        .bind(user_id)
        .execute(&mut **tx)
        .await?;
        let done: HashSet<String> = sqlx::query_scalar(
            "SELECT DISTINCT split_part(source_ref, ':', 2) FROM spans WHERE user_id = $1 AND source = 'playstation' AND source_ref LIKE 'est:%'",
        )
        .bind(user_id)
        .fetch_all(&mut **tx)
        .await?
        .into_iter()
        .collect();
        let mut written = 0;
        for game in games {
            let (Some(first), Some(last)) = (game.first_played_at, game.last_played_at) else {
                continue;
            };
            if done.contains(&game.title_id) {
                continue;
            }
            // Sessions observed live are already on the timeline; estimate only the rest.
            let (observed_count, observed_seconds, observed_from): (i64, i64, Option<DateTime<Utc>>) =
                sqlx::query_as(
                    "SELECT count(*), COALESCE(sum((data->>'duration_seconds')::bigint), 0)::bigint, min(start_at) \
                     FROM spans WHERE user_id = $1 AND source = 'playstation' AND data->>'title_id' = $2 AND source_ref NOT LIKE 'est:%'",
                )
                .bind(user_id)
                .bind(&game.title_id)
                .fetch_one(&mut **tx)
                .await?;
            let total = (game.play_duration_seconds as i64 - observed_seconds).max(0);
            if total == 0 {
                continue;
            }
            let count =
                (i64::from(game.play_count) - observed_count).clamp(1, MAX_ESTIMATED_SESSIONS);
            let end = observed_from.map_or(last, |from| from.min(last));
            let begin = first.min(end);
            let window = (end - begin).num_seconds();
            let each = if window > 0 {
                (total / count).min(window / count).max(1)
            } else {
                (total / count).max(1)
            };
            let hours = game.play_duration_seconds as f64 / 3600.0;
            for index in 0..count {
                let (start, finish) = if window > 0 && count == 1 {
                    (end - chrono::Duration::seconds(each), end)
                } else if window > 0 {
                    let step = (window - each) / (count - 1);
                    let start = if index == count - 1 {
                        end - chrono::Duration::seconds(each)
                    } else {
                        begin + chrono::Duration::seconds(index * step)
                    };
                    (start, start + chrono::Duration::seconds(each))
                } else {
                    let start = end - chrono::Duration::seconds((count - index) * each);
                    (start, start + chrono::Duration::seconds(each))
                };
                let data = json!({
                    "game": game.name,
                    "platform": game.platform,
                    "title_id": game.title_id,
                    "image_url": game.image_url,
                    "estimated": true,
                    "timing": "estimated_from_totals",
                    "estimated_session": index + 1,
                    "estimated_sessions": count,
                    "total_seconds": game.play_duration_seconds,
                    "connection_id": connection_id,
                });
                let notes = format!(
                    "Estimated session {} of {count} · {} · {hours:.1}h in total. PlayStation does not report exact session times.",
                    index + 1,
                    game.platform
                );
                let inserted = sqlx::query(
                    "INSERT INTO spans (user_id, title, notes, category, source, source_ref, status, start_at, end_at, execution_type, data, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, 'gaming', 'playstation', $4, 'done', $5, $6, 'manual_human', $7, 1, now(), now()) \
                     ON CONFLICT (user_id, source, source_ref) DO NOTHING",
                )
                .bind(user_id)
                .bind(&game.name)
                .bind(&notes)
                .bind(format!("est:{}:{index}", game.title_id))
                .bind(start)
                .bind(finish)
                .bind(&data)
                .execute(&mut **tx)
                .await?;
                written += inserted.rows_affected() as usize;
            }
        }
        notify(tx, user_id).await?;
        Ok(written)
    }

    async fn food_order(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        connection_id: Uuid,
        orders: &[FoodDeliveryOrder],
    ) -> Result<usize, FreshConnectionError> {
        let mut spans_created = 0;

        for order in orders {
            let Some(order_time) = order.order_time else {
                continue;
            };
            if order.status == FoodDeliveryStatus::Unknown {
                continue;
            }
            let source_ref = format!("{}:{}", order.provider, order.order_id);
            let span_status = match order.status {
                FoodDeliveryStatus::Delivered => "done",
                FoodDeliveryStatus::Cancelled => "cancelled",
                FoodDeliveryStatus::Unknown => unreachable!(),
                _ => "active",
            };

            let title = format!("{}: {}", order.provider_label(), order.restaurant_name);
            let notes = format!(
                "{} · {} · {}",
                order.items.join(", "),
                order
                    .total_amount
                    .map(|amount| match &order.currency {
                        Some(currency) => format!("{currency} {amount:.2}"),
                        None => format!("{amount:.2}"),
                    })
                    .unwrap_or_else(|| "Amount unavailable".into()),
                match order.status {
                    FoodDeliveryStatus::Unknown => "Status unavailable",
                    FoodDeliveryStatus::Placed => "Order placed",
                    FoodDeliveryStatus::Preparing => "Food being prepared",
                    FoodDeliveryStatus::OutForDelivery => "Out for delivery",
                    FoodDeliveryStatus::Delivered => "Delivered",
                    FoodDeliveryStatus::Cancelled => "Cancelled",
                }
            );

            let payload = json!({
                "order_id": order.order_id,
                "provider": order.provider,
                "restaurant_name": order.restaurant_name,
                "restaurant_location": order.restaurant_location,
                "delivery_address": order.delivery_address,
                "delivery_location": order.delivery_location,
                "rider_name": order.rider_name,
                "rider_phone": order.rider_phone,
                "rider_location": order.rider_location,
                "status": order.status.as_str(),
                "eta_minutes": order.eta_minutes,
                "total_amount": order.total_amount,
                "currency": order.currency,
                "items": order.items,
                "connection_id": connection_id,
                "provider_data": order.provider_data,
            });

            let event_id: Uuid = sqlx::query_scalar(
                "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, occurred_at, payload, processed_at) \
                 VALUES ($1, $2, $3, $4, $5, 'food_delivery.order_synced', $6, $7, now()) \
                 ON CONFLICT (source_kind, source_id, external_event_id) DO UPDATE SET occurred_at = EXCLUDED.occurred_at, payload = EXCLUDED.payload, payload_hash=EXCLUDED.payload_hash RETURNING id"
            )
            .bind(user_id)
            .bind(&order.provider)
            .bind(connection_id.to_string())
            .bind(&source_ref)
            .bind(format!("{:x}", sha2::Sha256::digest(serde_json::to_vec(&payload).unwrap_or_default())))
            .bind(order_time)
            .bind(&payload)
            .fetch_one(&mut **tx)
            .await?;

            let existing_id = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM spans WHERE user_id = $1 AND source = $2 AND source_ref = $3",
            )
            .bind(user_id)
            .bind(&order.provider)
            .bind(&source_ref)
            .fetch_optional(&mut **tx)
            .await?;

            let _span_id = if let Some(id) = existing_id {
                sqlx::query(
                    "UPDATE spans SET title = $1, notes = $2, start_at = $3, end_at = $4, status = $5, data = data || $6, source_event_id = $7, version = version + 1, updated_at = now() WHERE id = $8"
                )
                .bind(&title)
                .bind(&notes)
                .bind(order_time)
                .bind(order.delivered_time)
                .bind(span_status)
                .bind(&payload)
                .bind(event_id)
                .bind(id)
                .execute(&mut **tx)
                .await?;
                id
            } else {
                let id = sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO spans (user_id, title, notes, category, source, source_ref, status, start_at, end_at, execution_type, data, source_event_id, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, 'dining', $4, $5, $6, $7, $8, 'manual_human', $9, $10, 1, now(), now()) RETURNING id"
                )
                .bind(user_id)
                .bind(&title)
                .bind(&notes)
                .bind(&order.provider)
                .bind(&source_ref)
                .bind(span_status)
                .bind(order_time)
                .bind(order.delivered_time)
                .bind(&payload)
                .bind(event_id)
                .fetch_one(&mut **tx)
                .await?;
                spans_created += 1;
                id
            };
        }

        notify(tx, user_id).await?;

        Ok(spans_created)
    }
    fn food_committed(&self, user_id: Uuid, orders: &[FoodDeliveryOrder]) {
        let mut active_order_scene: Option<crate::map_scene::MapScene> = None;
        for order in orders {
            if order.status.is_active()
                && active_order_scene.is_none()
                && let (Some(rest), Some(cust)) =
                    (order.restaurant_location, order.delivery_location)
            {
                {
                    let Some(rider_pt) = order.rider_location else {
                        continue;
                    };
                    let pins = vec![
                        crate::map_scene::Pin {
                            id: format!("rest-{}", order.order_id),
                            lng: rest.lng,
                            lat: rest.lat,
                            label: Some(order.restaurant_name.clone()),
                            kind: crate::map_scene::PinKind::Place,
                            state: Some("Restaurant".into()),
                            span_id: None,
                        },
                        crate::map_scene::Pin {
                            id: format!("home-{}", order.order_id),
                            lng: cust.lng,
                            lat: cust.lat,
                            label: Some("Delivery Address".into()),
                            kind: crate::map_scene::PinKind::Place,
                            state: Some("Home".into()),
                            span_id: None,
                        },
                        crate::map_scene::Pin {
                            id: format!("rider-{}", order.order_id),
                            lng: rider_pt.lng,
                            lat: rider_pt.lat,
                            label: Some(format!("{} Delivery", order.provider_label())),
                            kind: crate::map_scene::PinKind::Task,
                            state: order
                                .eta_minutes
                                .map(|m| format!("{m} min away"))
                                .or_else(|| Some("In transit".into())),
                            span_id: None,
                        },
                    ];

                    let arcs = vec![crate::map_scene::Arc {
                        id: format!("delivery-arc-{}", order.order_id),
                        from: [rest.lng, rest.lat],
                        to: [cust.lng, cust.lat],
                        label: Some(format!("{} delivery", order.provider)),
                        delay_ms: 300,
                    }];

                    let camera = crate::map_scene::Camera {
                        lng: rider_pt.lng,
                        lat: rider_pt.lat,
                        zoom: Some(16.2),
                        pitch: Some(60.0),
                        bearing: Some(-28.0),
                        duration_ms: Some(2500),
                    };

                    let scene = crate::map_scene::MapScene {
                        rev: 1,
                        camera: Some(camera),
                        pins,
                        arcs,
                        columns: vec![],
                        highlights: vec![crate::map_scene::Highlight {
                            id: format!("hl-rest-{}", order.order_id),
                            lng: rest.lng,
                            lat: rest.lat,
                        }],
                        narration_hint: Some(format!(
                            "Your {} order from {}: {}{}",
                            order.provider_label(),
                            order.restaurant_name,
                            order.status.as_str(),
                            order
                                .eta_minutes
                                .map(|m| format!(", expected in {} minutes", m))
                                .unwrap_or_default()
                        )),
                    };

                    active_order_scene = Some(scene);
                }
            }
        }
        if let Some(hub) = &self.hub {
            if let Some(scene) = active_order_scene {
                hub.set_scene(user_id, scene);
            } else {
                // Show map data only when an active order is present
                let current = hub.scene(user_id);
                let is_delivery_scene = current.pins.iter().any(|p| {
                    p.id.starts_with("rider-")
                        || p.id.starts_with("rest-")
                        || p.id.starts_with("home-")
                });
                if is_delivery_scene {
                    hub.set_scene(user_id, crate::map_scene::MapScene::default());
                }
            }
        }
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
        hub: Option<crate::realtime::UserEventHub>,
        client: Option<String>,
        secret: Option<String>,
        core_url: Option<String>,
    ) -> Result<Self, FreshConnectionError> {
        Ok(Self(
            vox_connections::accounts::FreshConnectionsService::new(
                pool,
                credential_key,
                std::sync::Arc::new(CoreIngestor { hub }),
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
