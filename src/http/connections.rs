/**
* HTTP endpoints for managing external service connections.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    connections::{
        AuthorizeConnectionRequest, ConnectionError, InitiateConnectionRequest,
        VerifyConnectionCallbackRequest,
    },
    host_trust::{HostContextRequest, HostTrustError},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct Request {
    pub host_context: HostContextRequest,
    pub authorization: AuthorizeConnectionRequest,
}

#[derive(Deserialize)]
pub struct InitiateRequest {
    pub host_context: HostContextRequest,
    pub initiation: InitiateConnectionRequest,
}

#[derive(Deserialize)]
pub struct CallbackRequest {
    pub host_context: HostContextRequest,
    pub callback: VerifyConnectionCallbackRequest,
}

#[derive(Deserialize)]
pub struct ContextRequest {
    pub host_context: HostContextRequest,
}

pub async fn initiate(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<InitiateRequest>,
) -> Response {
    let (Some(trust), Some(connections)) = (s.host_trust.as_ref(), s.connections.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|x| x.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(v) => v,
        Err(HostTrustError::InvalidRequest) => return StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_) | HostTrustError::Identity(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match connections.initiate(&context, r.initiation).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(ConnectionError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConnectionError::IntegrationUnavailable) => StatusCode::NOT_FOUND.into_response(),
        Err(ConnectionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

pub async fn callback(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<CallbackRequest>,
) -> Response {
    let (Some(trust), Some(connections)) = (s.host_trust.as_ref(), s.connections.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|x| x.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(v) => v,
        Err(HostTrustError::InvalidRequest) => return StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_) | HostTrustError::Identity(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match connections.verify_callback(&context, r.callback).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(ConnectionError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(
            ConnectionError::Invalid
            | ConnectionError::InvalidState
            | ConnectionError::SessionExpired
            | ConnectionError::SessionAlreadyConsumed,
        ) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConnectionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

pub async fn authorize(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<Request>,
) -> Response {
    // A host assertion proves host identity, not a user's provider consent or
    // credential custody. Until a verified provider callback exists, a host
    // cannot create an authorized connection through this public endpoint.
    if matches!(
        r.authorization.authorization_state,
        crate::connections::AuthorizationState::Authorized
    ) || matches!(
        r.authorization.credential_custody,
        crate::connections::CredentialCustody::PlatformHeld
    ) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let (Some(trust), Some(connections)) = (s.host_trust.as_ref(), s.connections.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|x| x.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(v) => v,
        Err(HostTrustError::InvalidRequest) => return StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_) | HostTrustError::Identity(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match connections.record(&context, r.authorization).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(ConnectionError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConnectionError::IntegrationUnavailable | ConnectionError::NotFound) => {
            StatusCode::NOT_FOUND.into_response()
        }
        Err(ConnectionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}

pub async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ContextRequest>,
) -> Response {
    let (Some(trust), Some(connections)) = (s.host_trust.as_ref(), s.connections.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|x| x.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(v) => v,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match connections.list(&context).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn disconnect(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let (Some(trust), Some(connections)) = (s.host_trust.as_ref(), s.connections.as_ref()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|x| x.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(v) => v,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match connections.disconnect(&context, id).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(ConnectionError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ConnectionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::BAD_REQUEST.into_response(),
    }
}
