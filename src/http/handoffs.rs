/**
 * HTTP endpoints for labelled provider handoffs and discovery (E35, E37, E38).
 */
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::HostContextRequest,
    providers::{
        AmazonError, AmazonHandoffRequest, UberReadError, UberRideHandoffRequest, ZomatoError,
        ZomatoHandoffRequest,
    },
};
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
pub struct AmazonHandoffApiRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub handoff: AmazonHandoffRequest,
}

#[derive(Deserialize)]
pub struct ZomatoHandoffApiRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub handoff: ZomatoHandoffRequest,
}

#[derive(Deserialize)]
pub struct UberRideHandoffApiRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub handoff: UberRideHandoffRequest,
}

pub async fn amazon_handoff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AmazonHandoffApiRequest>,
) -> Response {
    let Some(service) = state.amazon.as_ref() else {
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
        .create_purchase_handoff(
            &context.request_context(),
            &request.agent_external_key,
            request.connection_id,
            request.handoff,
        )
        .await
    {
        Ok(handoff) => (StatusCode::OK, Json(handoff)).into_response(),
        Err(AmazonError::ConnectionNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(AmazonError::InvalidIntegration) => StatusCode::BAD_REQUEST.into_response(),
        Err(AmazonError::ReconnectRequired) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({
                "error": "reconnect_required",
                "message": "Connection expired or revoked; re-authorization required"
            })),
        )
            .into_response(),
        Err(AmazonError::UnauthorizedCapability(cap, agent)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "unauthorized_capability",
                "message": format!("Capability {} is not granted to agent {}", cap, agent)
            })),
        )
            .into_response(),
        Err(AmazonError::UnsupportedLocale(loc, supported)) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "unsupported_locale",
                "locale": loc,
                "supported_locales": supported
            })),
        )
            .into_response(),
        Err(AmazonError::UnsupportedDirectExecution(msg)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unsupported_direct_execution",
                "message": msg
            })),
        )
            .into_response(),
        Err(AmazonError::RateLimited(retry)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry.to_string())],
            Json(serde_json::json!({ "error": "rate_limited", "retry_after": retry })),
        )
            .into_response(),
        Err(AmazonError::ProviderError(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "provider_error", "details": err })),
        )
            .into_response(),
        Err(AmazonError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn zomato_handoff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ZomatoHandoffApiRequest>,
) -> Response {
    let Some(service) = state.zomato.as_ref() else {
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
        .create_order_handoff(
            &context.request_context(),
            &request.agent_external_key,
            request.connection_id,
            request.handoff,
        )
        .await
    {
        Ok(handoff) => (StatusCode::OK, Json(handoff)).into_response(),
        Err(ZomatoError::ConnectionNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ZomatoError::InvalidIntegration) => StatusCode::BAD_REQUEST.into_response(),
        Err(ZomatoError::MissingParameter) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "missing_parameter",
                "message": "Required identifier missing for requested handoff type"
            })),
        )
            .into_response(),
        Err(ZomatoError::ReconnectRequired) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({
                "error": "reconnect_required",
                "message": "Connection expired or revoked; re-authorization required"
            })),
        )
            .into_response(),
        Err(ZomatoError::UnauthorizedCapability(cap, agent)) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "unauthorized_capability",
                "message": format!("Capability {} is not granted to agent {}", cap, agent)
            })),
        )
            .into_response(),
        Err(ZomatoError::UnsupportedDirectExecution(msg)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unsupported_direct_execution",
                "message": msg
            })),
        )
            .into_response(),
        Err(ZomatoError::RateLimited(retry)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry.to_string())],
            Json(serde_json::json!({ "error": "rate_limited", "retry_after": retry })),
        )
            .into_response(),
        Err(ZomatoError::ProviderError(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "provider_error", "details": err })),
        )
            .into_response(),
        Err(ZomatoError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn uber_handoff(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<UberRideHandoffApiRequest>,
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
        .create_ride_handoff(
            &context.request_context(),
            &request.agent_external_key,
            request.connection_id,
            request.handoff,
        )
        .await
    {
        Ok(handoff) => (StatusCode::OK, Json(handoff)).into_response(),
        Err(UberReadError::ConnectionNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(UberReadError::InvalidIntegration) => StatusCode::BAD_REQUEST.into_response(),
        Err(UberReadError::ReconnectRequired) => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({
                "error": "reconnect_required",
                "message": "Connection expired or revoked; re-authorization required"
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
        Err(UberReadError::UnsupportedDirectExecution(msg)) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "unsupported_direct_execution",
                "message": msg
            })),
        )
            .into_response(),
        Err(UberReadError::RateLimited(retry)) => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry.to_string())],
            Json(serde_json::json!({ "error": "rate_limited", "retry_after": retry })),
        )
            .into_response(),
        Err(UberReadError::ProviderError(err)) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": "provider_error", "details": err })),
        )
            .into_response(),
        Err(UberReadError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
