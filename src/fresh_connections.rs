use chrono::{DateTime, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::collections::HashSet;
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
        let _category = match connector {
            "spotify" => "music",
            "youtube" => "video",
            "google_maps" => "visit",
            _ => {
                return Err(FreshConnectionError::Invalid(
                    "Unsupported activity source".into(),
                ));
            }
        };
        let mut created = 0;
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
            let source_ref = format!("{connector}:{}", item.source_id);
            let mut payload = json!({
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
            let _event_id: Uuid = sqlx::query_scalar(
                "INSERT INTO inbound_events (user_id,source_kind,source_id,external_event_id,payload_hash,event_type,occurred_at,payload,processed_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now()) \
                 ON CONFLICT (source_kind,source_id,external_event_id) DO UPDATE SET payload=EXCLUDED.payload,payload_hash=EXCLUDED.payload_hash,occurred_at=EXCLUDED.occurred_at RETURNING id"
            ).bind(user_id).bind(connector).bind(format!("{user_id}:{connector}"))
                .bind(&item.source_id)
                .bind(format!("{:x}", Sha256::digest(serde_json::to_vec(&event_payload).map_err(|_| FreshConnectionError::Invalid("Invalid activity payload".into()))?)))
                .bind(event_type).bind(item.occurred_at).bind(event_payload)
                .fetch_one(&mut **tx).await?;
            let type_val = match event_type {
                "music.listened" => "music", "place.visited" => "visit", "video.watched" => "video_watch",
                "video.liked" => "video_like", "video.playlist_added" => "video_playlist_addition", _ => "personal",
            };
            if connector == "spotify" {
                payload["track"] = json!(item.title);
                let artists = item.provider_data["artists"].as_array().map(|artists| artists.iter().filter_map(|artist| artist["name"].as_str()).collect::<Vec<_>>().join(", "));
                payload["artist"] = json!(artists);
                payload["album"] = item.provider_data["album"]["name"].clone();
            }
            if connector == "google_maps" { payload["place_name"] = json!(item.title); }
            if let Some(channel) = item.provider_data["channel"].as_str() { payload["channel"] = json!(channel); }
            validate_content(tx,type_val,&payload).await?;
            let event_row = sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, occurred_at, ended_at, time_precision, content, record_state, confidence, dedupe_key) \
                 SELECT $1, et.id, et.group_id, $2, $3, $4, $5, 'second', $6, 'active', 1.0, $7 \
                 FROM timeline_event_types et WHERE et.value = $8 AND et.owner_user_id IS NULL AND et.version = 1 AND et.state='published' \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                 title = EXCLUDED.title, summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, ended_at = EXCLUDED.ended_at, \
                 content = timeline_events.content || EXCLUDED.content, revision = timeline_events.revision + 1, updated_at = now() \
                 RETURNING id, (xmax = 0) AS inserted",
            )
            .bind(user_id)
            .bind(&title)
            .bind(&title)
            .bind(item.occurred_at)
            .bind(item.ended_at)
            .bind(&payload)
            .bind(&source_ref)
            .bind(type_val)
            .fetch_one(&mut **tx)
            .await?;

            let timeline_id: Uuid = event_row.get("id");
            let is_inserted: bool = event_row.get("inserted");

            sqlx::query(
                "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_type, source_id, raw_reference, observation_metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(timeline_id)
            .bind(user_id)
            .bind(connector)
            .bind(&item.source_id)
            .bind(&source_ref)
            .bind(&payload)
            .execute(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
                 VALUES ($1, 'connection_sync', $2, $2)",
            )
            .bind(user_id)
            .bind(item.occurred_at)
            .execute(&mut **tx)
            .await?;

            created += usize::from(is_inserted);
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
            let _span_status = if event.status == "cancelled" {
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

            let _event_id: Uuid = sqlx::query_scalar(
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

            let dedupe_key = format!("google_calendar:{}", event.id);
            let record_state = if event.status == "cancelled" {
                "retracted"
            } else {
                "active"
            };

            validate_content(tx,"appointment",&payload).await?;
            let event_row = sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, occurred_at, ended_at, time_precision, source_timezone, content, record_state, confidence, dedupe_key) \
                 SELECT $1, et.id, et.group_id, $2, $3, $4, $5, 'second', $6, $7, $8, 1.0, $9 \
                 FROM timeline_event_types et WHERE et.value = 'appointment' AND et.owner_user_id IS NULL AND et.version = 1 AND et.state='published' \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                 title = EXCLUDED.title, summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, ended_at = EXCLUDED.ended_at, \
                 source_timezone = EXCLUDED.source_timezone, content = timeline_events.content || EXCLUDED.content, \
                 record_state = EXCLUDED.record_state, revision = timeline_events.revision + 1, updated_at = now() \
                 RETURNING id, (xmax = 0) AS inserted",
            )
            .bind(user_id)
            .bind(&event.summary)
            .bind(&event.summary)
            .bind(event.start)
            .bind(Some(event.end))
            .bind(&event.time_zone)
            .bind(&payload)
            .bind(record_state)
            .bind(&dedupe_key)
            .fetch_one(&mut **tx)
            .await?;

            let timeline_id: Uuid = event_row.get("id");
            let is_inserted: bool = event_row.get("inserted");

            sqlx::query(
                "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_type, source_id, raw_reference, observation_metadata) \
                 VALUES ($1, $2, 'google_calendar', $3, $4, $5)",
            )
            .bind(timeline_id)
            .bind(user_id)
            .bind(&event.id)
            .bind(&source_ref)
            .bind(&payload)
            .execute(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
                 VALUES ($1, 'connection_sync', $2, $2)",
            )
            .bind(user_id)
            .bind(event.start)
            .execute(&mut **tx)
            .await?;

            created_or_updated += usize::from(is_inserted);
        }

        let existing_calendar_events = sqlx::query(
            "SELECT id, dedupe_key FROM timeline_events \
             WHERE user_id = $1 AND occurred_at >= $2 AND occurred_at < $3 AND record_state <> 'retracted' \
               AND content->>'account_id' = $4 AND dedupe_key LIKE 'google_calendar:%'",
        )
        .bind(user_id)
        .bind(time_min)
        .bind(time_max)
        .bind(account_id)
        .fetch_all(&mut **tx)
        .await?;

        for r in existing_calendar_events {
            let id: Uuid = r.get("id");
            let dkey: Option<String> = r.get("dedupe_key");
            if let Some(dk) = dkey {
                let calendar_ref = dk.strip_prefix("google_calendar:").unwrap_or(&dk);
                let sref = format!("{account_id}:primary:{calendar_ref}");
                if !seen_ids.contains(&sref) {
                    sqlx::query("UPDATE timeline_events SET record_state = 'retracted', revision = revision + 1, updated_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(&mut **tx)
                        .await?;
                }
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

                let _event_id: Uuid = sqlx::query_scalar(
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

                validate_content(tx,"gaming",&span_data).await?;
                let event_row = sqlx::query(
                    "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, occurred_at, ended_at, time_precision, content, record_state, confidence, dedupe_key) \
                     SELECT $1, et.id, et.group_id, $2, $3, $4, $5, 'second', $6, 'active', 1.0, $7 \
                     FROM timeline_event_types et WHERE et.value = 'gaming' AND et.owner_user_id IS NULL AND et.version = 1 AND et.state='published' \
                     ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                     title = EXCLUDED.title, summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, ended_at = EXCLUDED.ended_at, \
                     content = timeline_events.content || EXCLUDED.content, revision = timeline_events.revision + 1, updated_at = now() \
                     RETURNING id, (xmax = 0) AS inserted",
                )
                .bind(user_id)
                .bind(&title)
                .bind(&notes)
                .bind(act.observation_end)
                .bind(None::<DateTime<Utc>>)
                .bind(&span_data)
                .bind(&act.source_ref)
                .fetch_one(&mut **tx)
                .await?;

                let timeline_id: Uuid = event_row.get("id");
                let is_inserted: bool = event_row.get("inserted");

                sqlx::query(
                    "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_type, source_id, raw_reference, observation_metadata) \
                     VALUES ($1, $2, 'playstation', $3, $4, $5)",
                )
                .bind(timeline_id)
                .bind(user_id)
                .bind(&act.game.title_id)
                .bind(&act.source_ref)
                .bind(&span_data)
                .execute(&mut **tx)
                .await?;

                sqlx::query(
                    "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
                     VALUES ($1, 'connection_sync', $2, $2)",
                )
                .bind(user_id)
                .bind(act.observation_start)
                .execute(&mut **tx)
                .await?;

                spans_created += usize::from(is_inserted);
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
        let mut written = 0;
        for game in games {
            let (Some(first), Some(last)) = (game.first_played_at, game.last_played_at) else {
                continue;
            };
            let source_ref = format!("history:{}", game.title_id);
            let hours = game.play_duration_seconds as f64 / 3600.0;
            let data = json!({
                "game": game.name,
                "platform": game.platform,
                "title_id": game.title_id,
                "total_seconds": game.play_duration_seconds,
                "total_display": format!("{:.1}h", hours),
                "play_count": game.play_count,
                "first_played_at": first,
                "last_played_at": last,
                "timing": "cumulative_lifetime_stat",
                "is_cumulative_lifetime_stat": true,
                "session_times_known": false,
                "connection_id": connection_id,
            });
            let title = format!("PlayStation: {}", game.name);
            let notes = format!(
                "Lifetime playtime on {} · {:.1}h; {} provider play-count observations, session times unknown",
                game.platform, hours, game.play_count
            );
            validate_content(tx,"gaming",&data).await?;
            let event_row = sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, occurred_at, ended_at, time_precision, content, record_state, confidence, dedupe_key) \
                 SELECT $1, et.id, et.group_id, $2, $3, $4, $5, 'second', $6, 'active', 1.0, $7 \
                 FROM timeline_event_types et WHERE et.value = 'gaming' AND et.owner_user_id IS NULL AND et.version = 1 AND et.state='published' \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                 title = EXCLUDED.title, summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, ended_at = EXCLUDED.ended_at, \
                 content = timeline_events.content || EXCLUDED.content, revision = timeline_events.revision + 1, updated_at = now() \
                 RETURNING id, (xmax = 0) AS inserted",
            )
            .bind(user_id)
            .bind(&title)
            .bind(&notes)
            .bind(last)
            .bind(None::<DateTime<Utc>>)
            .bind(&data)
            .bind(&source_ref)
            .fetch_one(&mut **tx)
            .await?;

            let timeline_id: Uuid = event_row.get("id");
            let is_inserted: bool = event_row.get("inserted");

            sqlx::query(
                "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_type, source_id, raw_reference, observation_metadata) \
                 VALUES ($1, $2, 'playstation', $3, $4, $5)",
            )
            .bind(timeline_id)
            .bind(user_id)
            .bind(&game.title_id)
            .bind(&source_ref)
            .bind(&data)
            .execute(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
                 VALUES ($1, 'connection_sync', $2, $2)",
            )
            .bind(user_id)
            .bind(first)
            .execute(&mut **tx)
            .await?;

            written += usize::from(is_inserted);
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
            let _span_status = match order.status {
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

            let _event_id: Uuid = sqlx::query_scalar(
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

            let record_state = match order.status {
                FoodDeliveryStatus::Cancelled => "retracted",
                _ => "active",
            };

            validate_content(tx,"order",&payload).await?;
            let event_row = sqlx::query(
                "INSERT INTO timeline_events (user_id, event_type_id, group_id, title, summary, occurred_at, ended_at, time_precision, content, record_state, confidence, dedupe_key) \
                 SELECT $1, et.id, et.group_id, $2, $3, $4, $5, 'second', $6, $7, 1.0, $8 \
                 FROM timeline_event_types et WHERE et.value = 'order' AND et.owner_user_id IS NULL AND et.version = 1 AND et.state='published' \
                 ON CONFLICT (user_id, dedupe_key) WHERE dedupe_key IS NOT NULL DO UPDATE SET \
                 title = EXCLUDED.title, summary = EXCLUDED.summary, occurred_at = EXCLUDED.occurred_at, ended_at = EXCLUDED.ended_at, \
                 content = timeline_events.content || EXCLUDED.content, record_state = EXCLUDED.record_state, revision = timeline_events.revision + 1, updated_at = now() \
                 RETURNING id, (xmax = 0) AS inserted",
            )
            .bind(user_id)
            .bind(&title)
            .bind(&notes)
            .bind(order_time)
            .bind(order.delivered_time)
            .bind(&payload)
            .bind(record_state)
            .bind(&source_ref)
            .fetch_one(&mut **tx)
            .await?;

            let timeline_id: Uuid = event_row.get("id");
            let is_inserted: bool = event_row.get("inserted");

            sqlx::query(
                "INSERT INTO timeline_evidence (timeline_event_id, user_id, source_type, source_id, raw_reference, observation_metadata) \
                 VALUES ($1, $2, $3, $4, $5, $6)",
            )
            .bind(timeline_id)
            .bind(user_id)
            .bind(&order.provider)
            .bind(&order.order_id)
            .bind(&source_ref)
            .bind(&payload)
            .execute(&mut **tx)
            .await?;

            sqlx::query(
                "INSERT INTO pulse_invalidations (user_id, reason, range_start, range_end) \
                 VALUES ($1, 'connection_sync', $2, $2)",
            )
            .bind(user_id)
            .bind(order_time)
            .execute(&mut **tx)
            .await?;

            spans_created += usize::from(is_inserted);
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
    sqlx::query("SELECT pg_notify('vox_timeline_updated', $1)")
        .bind(json!({"type":"timeline_updated","user_id":user_id}).to_string())
        .execute(&mut **tx)
        .await?;
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

async fn validate_content(tx:&mut sqlx::Transaction<'_,sqlx::Postgres>,value:&str,content:&serde_json::Value) -> Result<(),FreshConnectionError> {
    crate::storage::timeline::validate_published_content(tx,value,content).await.map_err(|error| FreshConnectionError::Invalid(error.to_string()))
}
