use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;
use uuid::Uuid;
use vox_core::http::auth::authorized;

use crate::state::ApiState;

#[derive(Deserialize)]
pub struct DispatchDeviceRequest {
    pub user_id: Uuid,
    pub capability: String,
    pub kind: String,
    pub params: Value,
    pub timeout_secs: u64,
}

pub async fn dispatch_device_request(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Json(body): Json<DispatchDeviceRequest>,
) -> Response {
    if !authorized(&headers, state.legacy.service_token()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let device_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM devices \
         WHERE user_id = $1 AND is_active = true AND revoked_at IS NULL \
           AND capabilities->>$2 = 'true' \
         ORDER BY last_seen_at DESC LIMIT 1",
    )
    .bind(body.user_id)
    .bind(&body.capability)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);

    let Some(device_id) = device_id else {
        return StatusCode::NOT_FOUND.into_response();
    };

    let Some(link) = state.device_hub.get(device_id) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };

    let timeout = Duration::from_secs(body.timeout_secs.clamp(1, 120));
    match link.request(&body.kind, body.params, timeout).await {
        Ok(result) => Json(serde_json::json!({ "result": result })).into_response(),
        Err(_) => StatusCode::GATEWAY_TIMEOUT.into_response(),
    }
}
