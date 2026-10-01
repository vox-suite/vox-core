use axum::{Extension, Json, extract::State, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;
use vox_core::bridge_client::OutboundBridge;
use vox_core::domain::identity::Actor;
use vox_core::identity::UserId;
use vox_core::memory::MemoryService;
use vox_core::phone_verification::{PhoneVerificationError, PhoneVerificationService};

#[derive(Clone)]
pub struct PhoneApiState {
    pub pool: PgPool,
    pub memory: MemoryService,
    pub verification: PhoneVerificationService,
    pub bridge: Option<std::sync::Arc<dyn OutboundBridge>>,
}

#[derive(Debug, Deserialize)]
pub struct StartVerificationRequest {
    pub phone_number: String,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmVerificationRequest {
    pub phone_number: String,
    pub code: String,
}

fn verification_status(error: &PhoneVerificationError) -> StatusCode {
    match error {
        PhoneVerificationError::NotLinked => StatusCode::NOT_FOUND,
        PhoneVerificationError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        PhoneVerificationError::InvalidCode => StatusCode::BAD_REQUEST,
        PhoneVerificationError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub async fn start_phone_verification(
    Extension(actor): Extension<Actor>,
    State(state): State<PhoneApiState>,
    Json(payload): Json<StartVerificationRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let Some(bridge) = state.bridge.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let phone =
        vox_core::phone::normalize_e164(&payload.phone_number).ok_or(StatusCode::BAD_REQUEST)?;
    let issued = state
        .verification
        .issue(actor.user_id, &phone)
        .await
        .map_err(|e| verification_status(&e))?;
    bridge
        .send_verification_code(&phone, &issued.code)
        .await
        .map_err(|error| {
            tracing::warn!(%error, "Phone verification code was not delivered");
            StatusCode::BAD_GATEWAY
        })?;
    Ok(Json(serde_json::json!({ "expires_at": issued.expires_at })))
}

pub async fn confirm_phone_verification(
    Extension(actor): Extension<Actor>,
    State(state): State<PhoneApiState>,
    Json(payload): Json<ConfirmVerificationRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let phone =
        vox_core::phone::normalize_e164(&payload.phone_number).ok_or(StatusCode::BAD_REQUEST)?;
    state
        .verification
        .confirm(actor.user_id, &phone, &payload.code)
        .await
        .map_err(|e| verification_status(&e))?;
    Ok(Json(serde_json::json!({ "verified": true })))
}

const CTX_MERGE_TABLES: &[&str] = &[
    "auth_identities",
    "channel_identities",
    "conversations",
    "collections",
    "spans",
    "schedules",
    "jobs",
    "data_schemas",
    "devices",
    "connections",
    "action_proposals",
    "action_approvals",
    "executions",
    "inbound_events",
    "audit_events",
];

const USER_MERGE_TABLES: &[&str] = &[
    "collection_spans",
    "data_source_consents",
    "connected_app_pending_actions",
    "client_devices",
    "events",
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
    let normalized =
        vox_core::phone::normalize_e164(&payload.phone_number).ok_or(StatusCode::BAD_REQUEST)?;

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
            // Delete old auth sessions to avoid foreign key violations with auth_identities
            sqlx::query("DELETE FROM auth_sessions WHERE user_id = $1")
                .bind(old_user)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to delete old auth sessions: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

            let actor_context_id =
                sqlx::query_scalar::<_, Uuid>("SELECT id FROM user_contexts WHERE user_id = $1")
                    .bind(actor.user_id)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|e| {
                        tracing::error!("Failed to fetch actor user_context: {e}");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;

            for table in CTX_MERGE_TABLES {
                let sql = format!(
                    "UPDATE {table} SET user_id = $1, user_context_id = $2 WHERE user_id = $3"
                );
                sqlx::query(&sql)
                    .bind(actor.user_id)
                    .bind(actor_context_id)
                    .bind(old_user)
                    .execute(&mut *tx)
                    .await
                    .map_err(|e| {
                        tracing::error!("Failed to update {table}: {e}");
                        StatusCode::INTERNAL_SERVER_ERROR
                    })?;
            }

            for table in USER_MERGE_TABLES {
                let sql = format!("UPDATE {table} SET user_id = $1 WHERE user_id = $2");
                let _ = sqlx::query(&sql)
                    .bind(actor.user_id)
                    .bind(old_user)
                    .execute(&mut *tx)
                    .await;
            }

            sqlx::query("DELETE FROM user_contexts WHERE user_id = $1")
                .bind(old_user)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to delete old user_context: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;

            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(old_user)
                .execute(&mut *tx)
                .await
                .map_err(|e| {
                    tracing::error!("Failed to delete old user: {e}");
                    StatusCode::INTERNAL_SERVER_ERROR
                })?;
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
