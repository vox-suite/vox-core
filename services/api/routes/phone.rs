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
    pub phone_number: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConfirmVerificationRequest {
    pub phone_number: Option<String>,
    pub code: String,
}

async fn resolve_phone(
    pool: &PgPool,
    user_id: Uuid,
    provided: Option<&str>,
) -> Result<String, StatusCode> {
    match provided.map(str::trim).filter(|value| !value.is_empty()) {
        Some(raw) => vox_core::phone::normalize_e164(raw).ok_or(StatusCode::BAD_REQUEST),
        None => sqlx::query_scalar::<_, String>(
            "SELECT phone FROM (\
                SELECT normalized_phone AS phone, created_at, 0 AS rank \
                  FROM pending_phone_links WHERE user_id = $1 \
                UNION ALL \
                SELECT normalized_external_id, created_at, 1 FROM channel_identities \
                  WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL \
                    AND otp_verified_at IS NULL\
             ) candidates ORDER BY rank, created_at DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND),
    }
}

fn verification_status(error: &PhoneVerificationError) -> StatusCode {
    match error {
        PhoneVerificationError::NotLinked => StatusCode::NOT_FOUND,
        PhoneVerificationError::RateLimited => StatusCode::TOO_MANY_REQUESTS,
        PhoneVerificationError::InvalidCode => StatusCode::BAD_REQUEST,
        PhoneVerificationError::Conflict => StatusCode::CONFLICT,
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
    let phone = resolve_phone(&state.pool, actor.user_id, payload.phone_number.as_deref()).await?;
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
    let phone_last4: String = phone
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    Ok(Json(serde_json::json!({
        "expires_at": issued.expires_at,
        "phone_last4": phone_last4,
    })))
}

pub async fn confirm_phone_verification(
    Extension(actor): Extension<Actor>,
    State(state): State<PhoneApiState>,
    Json(payload): Json<ConfirmVerificationRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let phone = resolve_phone(&state.pool, actor.user_id, payload.phone_number.as_deref()).await?;
    let outcome = state
        .verification
        .confirm(actor.user_id, &phone, &payload.code)
        .await
        .map_err(|e| verification_status(&e))?;
    for guest in &outcome.merged_users {
        state.memory.forget_user(UserId(*guest)).await;
    }
    let _ = state
        .memory
        .refresh_minimal_user(UserId(actor.user_id))
        .await;
    Ok(Json(serde_json::json!({
        "verified": true,
        "merged": !outcome.merged_users.is_empty(),
    })))
}

#[derive(Debug, Deserialize)]
pub struct LinkPhoneRequest {
    pub phone_number: String,
}

#[derive(Debug, Serialize)]
pub struct LinkPhoneResponse {
    pub user_id: Uuid,
    pub merged: bool,
    pub pending: bool,
}

pub async fn link_phone(
    Extension(actor): Extension<Actor>,
    State(state): State<PhoneApiState>,
    Json(payload): Json<LinkPhoneRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let normalized =
        vox_core::phone::normalize_e164(&payload.phone_number).ok_or(StatusCode::BAD_REQUEST)?;
    let link_state = state
        .verification
        .begin_link(actor.user_id, &normalized)
        .await
        .map_err(|e| verification_status(&e))?;
    Ok(Json(LinkPhoneResponse {
        user_id: actor.user_id,
        merged: false,
        pending: link_state == vox_core::phone_verification::LinkState::Pending,
    }))
}
