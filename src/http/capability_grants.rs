/**
* HTTP endpoints for managing integration capability grants.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    capability_grants::{CapabilityGrantError, CreateGrantRequest},
    host_trust::{HostContextRequest, HostTrustError, HostTrustService},
    identity::ResolvedUserContext,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct GrantRequest {
    pub host_context: HostContextRequest,
    pub grant: CreateGrantRequest,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<GrantRequest>,
) -> Response {
    let (Some(trust), Some(grants)) = (state.host_trust.as_ref(), state.capability_grants.as_ref())
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_context(trust, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match grants.grant(&context, request.grant).await {
        Ok(grant) => (StatusCode::CREATED, Json(grant)).into_response(),
        Err(CapabilityGrantError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(CapabilityGrantError::Unavailable) => StatusCode::FORBIDDEN.into_response(),
        Err(CapabilityGrantError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn revoke(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<GrantRequest>,
) -> Response {
    let (Some(trust), Some(grants)) = (state.host_trust.as_ref(), state.capability_grants.as_ref())
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_context(trust, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match grants.revoke(&context, request.grant).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(CapabilityGrantError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(CapabilityGrantError::Unavailable) => StatusCode::NOT_FOUND.into_response(),
        Err(CapabilityGrantError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn effective(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(agent_external_key): Path<String>,
    Json(host_context): Json<HostContextRequest>,
) -> Response {
    let (Some(trust), Some(grants)) = (state.host_trust.as_ref(), state.capability_grants.as_ref())
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_context(trust, &headers, host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match grants
        .effective_for_agent(&context, &agent_external_key)
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(CapabilityGrantError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(CapabilityGrantError::Unavailable) => StatusCode::FORBIDDEN.into_response(),
        Err(CapabilityGrantError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn authenticated_context(
    trust: &HostTrustService,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<ResolvedUserContext> {
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    match trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
    {
        Ok(context) => Some(context),
        Err(
            HostTrustError::InvalidRequest
            | HostTrustError::Database(_)
            | HostTrustError::Identity(_),
        )
        | Err(_) => None,
    }
}
