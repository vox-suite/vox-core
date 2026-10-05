use super::{AppState, remote_extensions::context};
use crate::host_trust::HostContextRequest;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use uuid::Uuid;
use vox_connections::providers::playstation::PlayStationError;

#[derive(Deserialize)]
pub struct LinkRequest {
    pub host_context: HostContextRequest,
    pub npsso: String,
    pub capture_enabled: bool,
}
#[derive(Deserialize)]
pub struct CaptureRequest {
    pub host_context: HostContextRequest,
    pub capture_enabled: bool,
}
#[derive(Deserialize)]
pub struct ContextRequest {
    pub host_context: HostContextRequest,
}
fn error(e: PlayStationError) -> Response {
    let (status, code) = match e {
        PlayStationError::NotConfigured => (
            StatusCode::SERVICE_UNAVAILABLE,
            "playstation_not_configured",
        ),
        PlayStationError::ReconnectRequired => (StatusCode::UNAUTHORIZED, "reconnect_required"),
        PlayStationError::ConnectionNotFound => (StatusCode::NOT_FOUND, "connection_not_found"),
        PlayStationError::RateLimited(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
        PlayStationError::Invalid => (StatusCode::BAD_REQUEST, "invalid_playstation_request"),
        _ => (StatusCode::BAD_GATEWAY, "playstation_unavailable"),
    };
    (status, Json(serde_json::json!({"error":code}))).into_response()
}
pub async fn link(State(s): State<AppState>, h: HeaderMap, Json(r): Json<LinkRequest>) -> Response {
    let Some(context) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(capture) = s.playstation.as_ref() else {
        return error(PlayStationError::NotConfigured);
    };
    match capture
        .accounts
        .link(&context.request_context(), &r.npsso, r.capture_enabled)
        .await
    {
        Ok(connection) => (StatusCode::OK, Json(connection)).into_response(),
        Err(e) => error(e),
    }
}
pub async fn status(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(context) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(capture) = s.playstation.as_ref() else {
        return error(PlayStationError::NotConfigured);
    };
    match capture
        .accounts
        .status(&context.request_context(), id)
        .await
    {
        Ok(status) => Json(status).into_response(),
        Err(e) => error(e),
    }
}
pub async fn set_capture(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<CaptureRequest>,
) -> Response {
    let Some(context) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(capture) = s.playstation.as_ref() else {
        return error(PlayStationError::NotConfigured);
    };
    match capture
        .set_capture(&context.request_context(), id, r.capture_enabled)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => error(e),
    }
}
pub async fn sync(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(context) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(capture) = s.playstation.as_ref() else {
        return error(PlayStationError::NotConfigured);
    };
    match capture
        .sync_for_context(&context.request_context(), id)
        .await
    {
        Ok(result) => Json(result).into_response(),
        Err(e) => error(e),
    }
}
