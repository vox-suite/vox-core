/**
 * HTTP endpoints for consequential writes (Expedia Rapid Lodging) (E34).
 */
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::HostContextRequest,
    providers::{ExpediaLodgingError, ExpediaLodgingProposalDetails},
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ProposeLodgingBookingRequest {
    pub host_context: HostContextRequest,
    pub span_id: Uuid,
    pub task_run_id: Uuid,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub details: ExpediaLodgingProposalDetails,
    pub expires_at: DateTime<Utc>,
}

#[derive(Deserialize)]
pub struct ExecuteLodgingBookingRequest {
    pub host_context: HostContextRequest,
    pub approval_id: Uuid,
    pub idempotency_key: String,
}

#[derive(Deserialize)]
pub struct CancelLodgingBookingRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub itinerary_id: String,
    pub reason: String,
}

#[derive(Deserialize)]
pub struct ReconcileLodgingBookingRequest {
    pub host_context: HostContextRequest,
    pub affiliate_reference_id: String,
}

pub async fn propose(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ProposeLodgingBookingRequest>,
) -> Response {
    let Some(service) = state.expedia_write.as_ref() else {
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
        .propose_booking(
            &context,
            request.span_id,
            request.task_run_id,
            &request.agent_external_key,
            request.connection_id,
            request.details,
            request.expires_at,
        )
        .await
    {
        Ok(proposal) => (StatusCode::CREATED, Json(proposal)).into_response(),
        Err(e) => map_error(e),
    }
}

pub async fn execute(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ExecuteLodgingBookingRequest>,
) -> Response {
    let Some(service) = state.expedia_write.as_ref() else {
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
        .execute_booking(
            &context,
            request.approval_id,
            &request.idempotency_key,
            Utc::now(),
        )
        .await
    {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => map_error(e),
    }
}

pub async fn cancel(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CancelLodgingBookingRequest>,
) -> Response {
    let Some(service) = state.expedia_write.as_ref() else {
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
        .cancel_booking(
            &context,
            &request.agent_external_key,
            request.connection_id,
            &request.itinerary_id,
            &request.reason,
        )
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(e) => map_error(e),
    }
}

pub async fn reconcile(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ReconcileLodgingBookingRequest>,
) -> Response {
    let Some(service) = state.expedia_write.as_ref() else {
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
        .reconcile_booking(&context, &request.affiliate_reference_id, Utc::now())
        .await
    {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => map_error(e),
    }
}

fn map_error(err: ExpediaLodgingError) -> Response {
    match err {
        ExpediaLodgingError::ConnectionNotFound => StatusCode::NOT_FOUND.into_response(),
        ExpediaLodgingError::InvalidIntegration => StatusCode::BAD_REQUEST.into_response(),
        ExpediaLodgingError::InvalidProposal(msg) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "invalid_proposal",
                "message": msg
            })),
        )
            .into_response(),
        ExpediaLodgingError::ProposalNotFound => StatusCode::NOT_FOUND.into_response(),
        ExpediaLodgingError::ProposalExpired => (
            StatusCode::GONE,
            Json(serde_json::json!({
                "error": "proposal_expired",
                "message": "Action proposal has expired; a fresh proposal and approval are required"
            })),
        )
            .into_response(),
        ExpediaLodgingError::NotApproved => (
            StatusCode::PRECONDITION_FAILED,
            Json(serde_json::json!({
                "error": "not_approved",
                "message": "Proposal has not been approved or proposal details hash mismatched"
            })),
        )
            .into_response(),
        ExpediaLodgingError::ReconnectRequired => (
            StatusCode::PRECONDITION_REQUIRED,
            Json(serde_json::json!({
                "error": "reconnect_required",
                "message": "Connection expired or revoked; user re-authorization required"
            })),
        )
            .into_response(),
        ExpediaLodgingError::UnauthorizedCapability(cap, agent) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "unauthorized_capability",
                "message": format!("Capability {} is not granted to agent {}", cap, agent)
            })),
        )
            .into_response(),
        ExpediaLodgingError::RateLimited(retry_after) => (
            StatusCode::TOO_MANY_REQUESTS,
            [("Retry-After", retry_after.to_string())],
            Json(serde_json::json!({
                "error": "rate_limited",
                "retry_after_seconds": retry_after
            })),
        )
            .into_response(),
        ExpediaLodgingError::ProviderError(msg) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({
                "error": "provider_error",
                "details": msg
            })),
        )
            .into_response(),
        ExpediaLodgingError::ExecutionFailed(msg) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(serde_json::json!({
                "error": "execution_failed",
                "details": msg
            })),
        )
            .into_response(),
        ExpediaLodgingError::Timeout => (
            StatusCode::GATEWAY_TIMEOUT,
            Json(serde_json::json!({
                "error": "timeout",
                "message": "Provider request timed out; outcome remains reconciling"
            })),
        )
            .into_response(),
        ExpediaLodgingError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
