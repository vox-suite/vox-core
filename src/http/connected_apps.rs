/**
* HTTP endpoints for connecting remote MCP apps through provider OAuth.
*/
use super::{AppState, remote_extensions::context};
use crate::{connected_apps::ConnectedAppError, host_trust::HostContextRequest};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct AuthorizeRequest {
    pub host_context: HostContextRequest,
    pub redirect_uri: String,
}

#[derive(Deserialize)]
pub struct CallbackRequest {
    pub host_context: HostContextRequest,
    pub state: String,
    pub code: String,
}

#[derive(Deserialize)]
pub struct StatusRequest {
    pub host_context: HostContextRequest,
}

pub async fn authorize(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<AuthorizeRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps.begin(&c, id, &r.redirect_uri).await {
        Ok(start) => (StatusCode::OK, Json(start)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn callback(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<CallbackRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps.complete(&c, &r.state, &r.code).await {
        Ok(extension) => (StatusCode::OK, Json(extension)).into_response(),
        Err(e) => error(e),
    }
}

pub async fn status(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<StatusRequest>,
) -> Response {
    let Some(apps) = s.connected_apps.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match apps.connections(&c).await {
        Ok(connected) => (
            StatusCode::OK,
            Json(json!({
                "configured_hosts": apps.configured_hosts(),
                "connected": connected,
            })),
        )
            .into_response(),
        Err(e) => error(e),
    }
}

/// Errors carry a short user-facing reason; provider details stay in logs.
fn error(e: ConnectedAppError) -> Response {
    let (status, code) = match &e {
        ConnectedAppError::Invalid => (StatusCode::BAD_REQUEST, "invalid_request"),
        ConnectedAppError::NotFound => (StatusCode::NOT_FOUND, "not_found"),
        ConnectedAppError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, "not_configured"),
        ConnectedAppError::ClientNotConfigured => {
            (StatusCode::PRECONDITION_FAILED, "client_not_configured")
        }
        ConnectedAppError::Unauthorized => (StatusCode::BAD_GATEWAY, "provider_rejected"),
        ConnectedAppError::Expired => (StatusCode::GONE, "authorization_expired"),
        ConnectedAppError::Provider(_)
        | ConnectedAppError::SessionExpired
        | ConnectedAppError::UnknownTool => (StatusCode::BAD_GATEWAY, "provider_error"),
        ConnectedAppError::Timeout => (StatusCode::GATEWAY_TIMEOUT, "provider_error"),
        ConnectedAppError::NoPendingAction => (StatusCode::GONE, "not_found"),
        ConnectedAppError::Extension(_) => (StatusCode::CONFLICT, "extension_state"),
        ConnectedAppError::Crypto | ConnectedAppError::Database(_) => {
            (StatusCode::SERVICE_UNAVAILABLE, "unavailable")
        }
    };
    tracing::warn!(error = %e, code, "connected app request failed");
    (
        status,
        Json(json!({"error": code, "message": e.to_string()})),
    )
        .into_response()
}
