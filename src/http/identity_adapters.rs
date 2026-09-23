/**
* HTTP endpoints for third-party identity provider resolution.
*/
use super::{AppState, auth, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustError},
    identity_adapters::{
        AuthenticateIdentityRequest, IdentityAdapterError, RegisterIdentityAdapterRequest,
        StartPasswordlessRecoveryRequest,
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

#[derive(Deserialize)]
pub struct ContextAuthenticationRequest {
    pub host_context: HostContextRequest,
    pub authentication: AuthenticateIdentityRequest,
}

#[derive(Deserialize)]
pub struct ContextRecoveryRequest {
    pub host_context: HostContextRequest,
    pub recovery: StartPasswordlessRecoveryRequest,
}

#[derive(Deserialize)]
pub struct IdentityLinkRequest {
    pub source_authentication_token: String,
    pub target_authentication_token: String,
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterIdentityAdapterRequest>,
) -> Response {
    let Some(service) = state.identity_adapters.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.register_adapter(request).await {
        Ok(adapter) => (StatusCode::CREATED, Json(adapter)).into_response(),
        Err(IdentityAdapterError::InvalidRegistration) => StatusCode::BAD_REQUEST.into_response(),
        Err(IdentityAdapterError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn start_passwordless_recovery(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContextRecoveryRequest>,
) -> Response {
    let context = match resolve_context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(error) => return host_context_error(error),
    };
    let Some(service) = state.identity_adapters.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .start_passwordless_recovery(&context, request.recovery, Utc::now())
        .await
    {
        Ok(started) => (StatusCode::ACCEPTED, Json(started)).into_response(),
        Err(error) => adapter_error(error),
    }
}

pub async fn authenticate(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContextAuthenticationRequest>,
) -> Response {
    let context = match resolve_context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(error) => return host_context_error(error),
    };
    let Some(service) = state.identity_adapters.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .authenticate(&context, request.authentication, Utc::now())
        .await
    {
        Ok(result) => (StatusCode::OK, Json(result)).into_response(),
        Err(error) => adapter_error(error),
    }
}

pub async fn link(
    State(state): State<AppState>,
    Json(request): Json<IdentityLinkRequest>,
) -> Response {
    let Some(service) = state.identity_adapters.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .link_identities(
            &request.source_authentication_token,
            &request.target_authentication_token,
            Utc::now(),
        )
        .await
    {
        Ok(result) => (StatusCode::CREATED, Json(result)).into_response(),
        Err(error) => adapter_error(error),
    }
}

pub async fn unlink(
    State(state): State<AppState>,
    Json(request): Json<IdentityLinkRequest>,
) -> Response {
    let Some(service) = state.identity_adapters.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .unlink_identities(
            &request.source_authentication_token,
            &request.target_authentication_token,
            Utc::now(),
        )
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => adapter_error(error),
    }
}

async fn resolve_context(
    state: &AppState,
    headers: &HeaderMap,
    request: &HostContextRequest,
) -> Result<crate::identity::ResolvedUserContext, HostTrustError> {
    let host_trust = state
        .host_trust
        .as_ref()
        .ok_or(HostTrustError::Database(sqlx::Error::PoolClosed))?;
    let assertion = assertion_from_headers(headers)?;
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    host_trust
        .resolve_authenticated_context(&assertion, request, origin, Utc::now())
        .await
}

fn host_context_error(error: HostTrustError) -> Response {
    match error {
        HostTrustError::InvalidRequest => StatusCode::BAD_REQUEST.into_response(),
        HostTrustError::Database(_) | HostTrustError::Identity(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

fn adapter_error(error: IdentityAdapterError) -> Response {
    match error {
        IdentityAdapterError::InvalidRegistration
        | IdentityAdapterError::InvalidProof
        | IdentityAdapterError::SameIdentity => StatusCode::BAD_REQUEST.into_response(),
        IdentityAdapterError::AdapterUnavailable
        | IdentityAdapterError::RecoveryUnavailable
        | IdentityAdapterError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        IdentityAdapterError::LinkNotFound => StatusCode::NOT_FOUND.into_response(),
        IdentityAdapterError::ProofExpired
        | IdentityAdapterError::ProofReplayed
        | IdentityAdapterError::AuthenticationDenied => StatusCode::UNAUTHORIZED.into_response(),
    }
}
