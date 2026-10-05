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
            other => other,
        };
        let category = match connector {
            "spotify" => "music",
            "youtube" => "video",
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
            let event_id: Uuid = sqlx::query_scalar(
                "INSERT INTO inbound_events (user_id,source_kind,source_id,external_event_id,payload_hash,event_type,occurred_at,payload,processed_at) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,now()) \
                 ON CONFLICT (source_kind,source_id,external_event_id) DO UPDATE SET payload=EXCLUDED.payload,payload_hash=EXCLUDED.payload_hash,occurred_at=EXCLUDED.occurred_at RETURNING id"
            ).bind(user_id).bind(connector).bind(format!("{user_id}:{connector}"))
                .bind(&item.source_id)
                .bind(format!("{:x}", Sha256::digest(serde_json::to_vec(&event_payload).map_err(|_| FreshConnectionError::Invalid("Invalid activity payload".into()))?)))
                .bind(event_type).bind(item.occurred_at).bind(event_payload)
                .fetch_one(&mut **tx).await?;
            let inserted: bool = sqlx::query_scalar(
                "INSERT INTO spans (user_id,title,category,source,source_ref,status,start_at,end_at,execution_type,data,source_event_id) \
                 VALUES ($1,$2,$3,$4,$5,'done',$6,$7,'manual_human',$8,$9) \
                 ON CONFLICT (user_id,source,source_ref) DO UPDATE SET \
                 title=EXCLUDED.title,start_at=EXCLUDED.start_at,end_at=EXCLUDED.end_at, \
                 data=spans.data || EXCLUDED.data,source_event_id=EXCLUDED.source_event_id,version=spans.version+1,updated_at=now() \
                 WHERE (spans.title,spans.start_at,spans.end_at,spans.data || EXCLUDED.data,spans.source_event_id) \
                 IS DISTINCT FROM (EXCLUDED.title,EXCLUDED.start_at,EXCLUDED.end_at,spans.data,EXCLUDED.source_event_id) RETURNING (xmax=0)"
            ).bind(user_id).bind(&title).bind(category).bind(connector).bind(source_ref)
                .bind(item.occurred_at).bind(item.ended_at).bind(payload).bind(event_id)
                .fetch_optional(&mut **tx).await?.unwrap_or(false);
            created += usize::from(inserted);
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
            let end = last.max(first);
            let source_ref = format!("history:{}", game.title_id);
            let hours = game.play_duration_seconds as f64 / 3600.0;
            let data = json!({
                "game": game.name,
                "platform": game.platform,
                "title_id": game.title_id,
                "total_seconds": game.play_duration_seconds,
                "total_display": format!("{:.1}h", hours),
                "play_count": game.play_count,
                "timing": "first_to_last_played",
                "connection_id": connection_id,
            });
            let title = format!("PlayStation: {}", game.name);
            let notes = format!(
                "Played on {} · {:.1}h in total across {} sessions",
                game.platform, hours, game.play_count
            );
            let existing = sqlx::query_scalar::<_, Uuid>(
                "SELECT id FROM spans WHERE user_id = $1 AND source = 'playstation' AND source_ref = $2",
            )
            .bind(user_id)
            .bind(&source_ref)
            .fetch_optional(&mut **tx)
            .await?;
            if let Some(id) = existing {
                sqlx::query(
                    "UPDATE spans SET title = $2, notes = $3, start_at = $4, end_at = $5, data = $6, version = version + 1, updated_at = now() \
                     WHERE id = $1 AND (start_at IS DISTINCT FROM $4 OR end_at IS DISTINCT FROM $5 OR data IS DISTINCT FROM $6)",
                )
                .bind(id)
                .bind(&title)
                .bind(&notes)
                .bind(first)
                .bind(end)
                .bind(&data)
                .execute(&mut **tx)
                .await?;
            } else {
                sqlx::query(
                    "INSERT INTO spans (user_id, title, notes, category, source, source_ref, status, start_at, end_at, execution_type, data, version, created_at, updated_at) \
                     VALUES ($1, $2, $3, 'gaming', 'playstation', $4, 'done', $5, $6, 'manual_human', $7, 1, now(), now())",
                )
                .bind(user_id)
                .bind(&title)
                .bind(&notes)
                .bind(&source_ref)
                .bind(first)
                .bind(end)
                .bind(&data)
                .execute(&mut **tx)
                .await?;
                written += 1;
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
