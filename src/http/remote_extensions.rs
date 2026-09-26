/**
* HTTP endpoints for remote extension governance, conformance, and renewed consent.
*/
use super::{AppState, auth, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustService},
    remote_extensions::{
        InstallExtensionRequest, RemoteExtension, RemoteExtensionError, UpdateExtensionRequest,
    },
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
pub struct ContextRequest {
    pub host_context: HostContextRequest,
}

#[derive(Deserialize)]
pub struct InstallRequest {
    pub host_context: HostContextRequest,
    pub extension: InstallExtensionRequest,
}

#[derive(Deserialize)]
pub struct UpdateRequest {
    pub host_context: HostContextRequest,
    pub extension: UpdateExtensionRequest,
}

#[derive(Deserialize)]
pub struct SetEnabledRequest {
    pub host_context: HostContextRequest,
    pub enabled: bool,
}

#[derive(Deserialize)]
pub struct ConformanceReportRequest {
    pub host_context: HostContextRequest,
    pub version: i32,
    pub passed: bool,
    pub report: serde_json::Value,
}

#[derive(Deserialize)]
pub struct RenewConsentRequest {
    pub host_context: HostContextRequest,
    pub version: i32,
}

#[derive(Deserialize)]
pub struct QuarantineRequest {
    pub host_context: HostContextRequest,
    pub version: i32,
}

pub async fn install(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<InstallRequest>,
) -> Response {
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.install(&c, r.extension).await, StatusCode::CREATED)
}

pub async fn list(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.list(&c).await {
        Ok(list) => (StatusCode::OK, Json(list)).into_response(),
        Err(e) => reply_error(e),
    }
}

pub async fn get(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.get(&c, id).await, StatusCode::OK)
}

pub async fn update(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<UpdateRequest>,
) -> Response {
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.update(&c, id, r.extension).await, StatusCode::OK)
}

pub async fn set_enabled(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<SetEnabledRequest>,
) -> Response {
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(
        service.set_operator_enabled(&c, id, r.enabled).await,
        StatusCode::OK,
    )
}

pub async fn record_conformance(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ConformanceReportRequest>,
) -> Response {
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(
        service
            .record_conformance(&c, id, r.version, r.passed, r.report)
            .await,
        StatusCode::OK,
    )
}

pub async fn renew_consent(
    State(_s): State<AppState>,
    _h: HeaderMap,
    Path(_id): Path<Uuid>,
    Json(_r): Json<RenewConsentRequest>,
) -> Response {
    // A host assertion proves identity, not that the user reviewed the
    // operator/recipient diff. Keep renewal unavailable until the public
    // consent flow can bind an explicit decision to the reviewed version.
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

pub async fn quarantine(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<QuarantineRequest>,
) -> Response {
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::FORBIDDEN.into_response();
    }
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.quarantine(&c, id, r.version).await, StatusCode::OK)
}

pub async fn remove(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(service) = s.remote_extensions.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(service.remove(&c, id).await, StatusCode::OK)
}

fn reply(r: Result<RemoteExtension, RemoteExtensionError>, ok: StatusCode) -> Response {
    match r {
        Ok(v) => (ok, Json(v)).into_response(),
        Err(e) => reply_error(e),
    }
}

fn reply_error(e: RemoteExtensionError) -> Response {
    match e {
        RemoteExtensionError::Invalid | RemoteExtensionError::LocalCodeProhibited => {
            StatusCode::BAD_REQUEST.into_response()
        }
        RemoteExtensionError::NotFound => StatusCode::NOT_FOUND.into_response(),
        RemoteExtensionError::Conflict => StatusCode::CONFLICT.into_response(),
        RemoteExtensionError::NotActive(_) | RemoteExtensionError::Quarantined => {
            StatusCode::FORBIDDEN.into_response()
        }
        RemoteExtensionError::ConsequentialUnavailable => {
            StatusCode::PRECONDITION_FAILED.into_response()
        }
        RemoteExtensionError::ConsentRequired => StatusCode::PRECONDITION_REQUIRED.into_response(),
        RemoteExtensionError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

async fn context(
    trust: Option<&HostTrustService>,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust = trust?;
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
        .ok()
}
