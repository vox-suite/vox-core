/**
* HTTP handlers for event ingestion and telemetry tracking.
*/
use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::domain::identity::Actor;

#[derive(Debug, Deserialize)]
pub struct BatchEventItem {
    pub event_type: String,
    pub payload: Option<serde_json::Value>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub external_event_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BatchEventsRequest {
    pub events: Vec<BatchEventItem>,
}

#[derive(Debug, Serialize)]
pub struct BatchEventsResponse {
    pub inserted: usize,
}

pub async fn ingest_batch(
    State(pool): State<PgPool>,
    Extension(actor): Extension<Actor>,
    Json(body): Json<BatchEventsRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    if body.events.is_empty() {
        return Ok(Json(BatchEventsResponse { inserted: 0 }));
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut count = 0;
    let batch_id = Uuid::new_v4();

    for item in body.events {
        let event_type = item.event_type.trim();
        if event_type.is_empty() {
            continue;
        }
        let payload = item.payload.unwrap_or_else(|| serde_json::json!({}));
        let occurred_at = item.occurred_at.unwrap_or_else(Utc::now);
        let source_kind = if actor.device_id().is_some() {
            "device"
        } else {
            "user"
        };
        let source_id = actor.device_id().unwrap_or(actor.user_id).to_string();
        let external_event_id = item
            .external_event_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        let payload_bytes = serde_json::to_vec(&payload).unwrap_or_default();
        let payload_hash = hex::encode(Sha256::digest(payload_bytes));

        let inserted = sqlx::query(
            "INSERT INTO inbound_events (user_id, source_kind, source_id, external_event_id, payload_hash, event_type, payload, occurred_at, batch_id) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (source_kind, source_id, external_event_id) DO NOTHING",
        )
        .bind(actor.user_id)
        .bind(source_kind)
        .bind(source_id)
        .bind(external_event_id)
        .bind(payload_hash)
        .bind(event_type)
        .bind(payload)
        .bind(occurred_at)
        .bind(batch_id)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        count += usize::try_from(inserted.rows_affected()).unwrap_or(0);
    }

    if count > 0 {
        sqlx::query("INSERT INTO jobs(kind,user_id,payload_reference_id) VALUES('process_event_batch',$1,$2)")
            .bind(actor.user_id).bind(batch_id).execute(&mut *tx).await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(BatchEventsResponse { inserted: count }))
}

#[cfg(test)]
mod regression_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires disposable TEST_DATABASE_URL with pgvector"]
    async fn batch_route_schedules_processing_atomically() {
        let pool = PgPool::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        let user: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let db = vox_core::db::Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        vox_core::identity::IdentityService::new(db)
            .resolve_for_user(user)
            .await
            .unwrap();
        ingest_batch(
            State(pool.clone()),
            Extension(Actor::user(user)),
            Json(BatchEventsRequest {
                events: vec![BatchEventItem {
                    event_type: "activity".into(),
                    payload: None,
                    occurred_at: None,
                    external_event_id: Some("stable".into()),
                }],
            }),
        )
        .await
        .unwrap();
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM jobs j JOIN inbound_events e ON j.payload_reference_id=e.batch_id AND j.user_id=e.user_id WHERE e.user_id=$1 AND j.kind='process_event_batch'").bind(user).fetch_one(&pool).await.unwrap();
        assert_eq!(count, 1);
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }
}
