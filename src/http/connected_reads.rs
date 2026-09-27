/**
* HTTP endpoint for connected reads (E33).
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{host_trust::HostContextRequest, providers::UberReadError};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ConnectedReadRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub capability_external_key: String,
    #[serde(default)]
    pub offset: usize,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_limit() -> usize {
    20
}

pub async fn read(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ConnectedReadRequest>,
) -> Response {
    let Some(service) = state.uber_read.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    match service
        .read_history(
            &context.request_context(),
            &request.agent_external_key,
            request.connection_id,
            &request.capability_external_key,
            request.offset,
            request.limit,
        )
        .await
    {
        Ok(history) => (StatusCode::OK, Json(history)).into_response(),
        Err(UberReadError::ConnectionNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(UberReadError::InvalidIntegration) => StatusCode::BAD_REQUEST.into_response(),
        Err(UberReadError::ReconnectRequired) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({
                "error": "reconnect_required",
                "message": "Connection expired or revoked; user re-authorization required"
            })),
        )
            .into_response(),
        Err(UberReadError::UnauthorizedCapability(cap, agent)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "unauthorized_capability",
                "message": format!("Capability {} is not granted to agent {}", cap, agent)
            })),
        )
            .into_response(),
        Err(UberReadError::RateLimited(retry_after)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry_after.to_string())],
            Json(serde_json::json!({
                "error": "rate_limited",
                "retry_after_seconds": retry_after
            })),
        )
            .into_response(),
        Err(UberReadError::ProviderError(msg)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "provider_error",
                "details": msg
            })),
        )
            .into_response(),
        Err(UberReadError::UnsupportedDirectExecution(msg)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unsupported_direct_execution",
                "message": msg
            })),
        )
            .into_response(),
        Err(UberReadError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
