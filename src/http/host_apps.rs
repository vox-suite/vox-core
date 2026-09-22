/**
* HTTP endpoints for registering and communicating with host applications.
*/
use super::{AppState, auth};
use crate::host_trust::{
    HostContextAssertion, HostContextRequest, HostTrustError, RegisterHostAppRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use uuid::Uuid;

const CREDENTIAL_HEADER: &str = "x-vox-host-credential";
const SECRET_HEADER: &str = "x-vox-host-secret";
const AUDIENCE_HEADER: &str = "x-vox-host-audience";
const TIMESTAMP_HEADER: &str = "x-vox-host-timestamp";
const NONCE_HEADER: &str = "x-vox-host-nonce";
const SIGNATURE_HEADER: &str = "x-vox-host-signature";

#[derive(Serialize)]
struct HostContextResponse {
    user_context_id: crate::identity::UserContextId,
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterHostAppRequest>,
) -> Response {
    let Some(host_trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match host_trust.register_host_app(request).await {
        Ok(host) => (StatusCode::CREATED, Json(host)).into_response(),
        Err(HostTrustError::InvalidRegistration) => StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn rotate_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(host_app_id): Path<String>,
) -> Response {
    let Some(host_trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(host_app_id) = Uuid::parse_str(&host_app_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match host_trust
        .rotate_credential(crate::identity::HostAppId(host_app_id))
        .await
    {
        Ok(credential) => (StatusCode::CREATED, Json(credential)).into_response(),
        Err(HostTrustError::CredentialNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(HostTrustError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn revoke_credential(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(credential_id): Path<String>,
) -> Response {
    let Some(host_trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(credential_id) = Uuid::parse_str(&credential_id) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match host_trust.revoke_credential(credential_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(HostTrustError::CredentialNotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(HostTrustError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn resolve_context(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HostContextRequest>,
) -> Response {
    let Some(host_trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(assertion) => assertion,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    match host_trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
    {
        Ok(context) => (
            StatusCode::OK,
            Json(HostContextResponse {
                user_context_id: context.id,
            }),
        )
            .into_response(),
        Err(HostTrustError::InvalidRequest) => StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_) | HostTrustError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
        Err(_) => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub(crate) fn assertion_from_headers(
    headers: &HeaderMap,
) -> Result<HostContextAssertion, HostTrustError> {
    let credential_id = header(headers, CREDENTIAL_HEADER)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(HostTrustError::InvalidAssertion)?;
    let audience = header(headers, AUDIENCE_HEADER)
        .map(str::to_owned)
        .ok_or(HostTrustError::InvalidAssertion)?;
    let issued_at = header(headers, TIMESTAMP_HEADER)
        .and_then(|value| value.parse::<i64>().ok())
        .and_then(|timestamp| DateTime::from_timestamp(timestamp, 0))
        .ok_or(HostTrustError::InvalidAssertion)?;
    let nonce = header(headers, NONCE_HEADER)
        .and_then(|value| Uuid::parse_str(value).ok())
        .ok_or(HostTrustError::InvalidAssertion)?;
    let signature = header(headers, SIGNATURE_HEADER)
        .map(str::to_owned)
        .ok_or(HostTrustError::InvalidAssertion)?;
    let secret = header(headers, SECRET_HEADER)
        .map(str::to_owned)
        .ok_or(HostTrustError::InvalidAssertion)?;
    Ok(HostContextAssertion::from_parts(
        credential_id,
        audience,
        issued_at,
        nonce,
        signature,
        secret,
    ))
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .filter(|value| !value.is_empty())
}
