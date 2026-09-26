use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::domain::identity::Actor;
use vox_core::identity::UserId;
use vox_core::memory::MemoryService;

#[derive(Clone)]
pub struct PhoneApiState {
    pub pool: PgPool,
    pub memory: MemoryService,
}

const MERGE_TABLES: &[&str] = &[
    "auth_identities",
    "channel_identities",
    "auth_sessions",
    "conversations",
    "collections",
    "spans",
    "collection_spans",
    "schedules",
    "jobs",
    "data_schemas",
    "records",
    "devices",
    "connections",
    "action_proposals",
    "action_approvals",
    "executions",
    "inbound_events",
    "audit_events",
    "user_contexts",
];

#[derive(Debug, Deserialize)]
pub struct LinkPhoneRequest {
    pub phone_number: String,
}

#[derive(Debug, Serialize)]
pub struct LinkPhoneResponse {
    pub user_id: Uuid,
    pub merged: bool,
}

pub async fn link_phone(
    Extension(actor): Extension<Actor>,
    State(state): State<PhoneApiState>,
    Json(payload): Json<LinkPhoneRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let pool = &state.pool;
    let normalized: String = payload
        .phone_number
        .chars()
        .filter(|c| c.is_ascii_digit())
        .collect();
    if normalized.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let mut tx = pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let existing_owner = sqlx::query_scalar::<_, Uuid>(
        "SELECT user_id FROM channel_identities \
         WHERE channel = 'phone' AND normalized_external_id = $1 AND revoked_at IS NULL",
    )
    .bind(&normalized)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let merged = match existing_owner {
        None => {
            sqlx::query(
                "INSERT INTO channel_identities (user_id, channel, normalized_external_id, verified_at) \
                 VALUES ($1, 'phone', $2, now())",
            )
            .bind(actor.user_id)
            .bind(&normalized)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            false
        }
        Some(owner) if owner == actor.user_id => false,
        Some(old_user) => {
            for table in MERGE_TABLES {
                let sql = format!("UPDATE {table} SET user_id = $1 WHERE user_id = $2");
                sqlx::query(&sql)
                    .bind(actor.user_id)
                    .bind(old_user)
                    .execute(&mut *tx)
                    .await
                    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            }
            sqlx::query("UPDATE users SET status = 'disabled' WHERE id = $1")
                .bind(old_user)
                .execute(&mut *tx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            true
        }
    };

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if !matches!(existing_owner, Some(owner) if owner == actor.user_id) {
        // The phone -> user Redis index is keyed by (channel, external_id), so
        // refreshing the new owner overwrites any stale mapping left by a
        // previous linked/anonymous owner without needing to touch their record.
        let _ = state
            .memory
            .refresh_minimal_user(UserId(actor.user_id))
            .await;
    }

    Ok(Json(LinkPhoneResponse {
        user_id: actor.user_id,
        merged,
    }))
}
