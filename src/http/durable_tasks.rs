/**
* HTTP endpoints for scheduling and tracking durable tasks.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    durable_tasks::{DurableTaskError, StartTaskRequest, WaitRequest},
    host_trust::{HostContextRequest, HostTrustService},
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
pub struct StartRequest {
    pub host_context: HostContextRequest,
    pub task: StartTaskRequest,
}

#[derive(Deserialize)]
pub struct WaitBody {
    pub host_context: HostContextRequest,
    pub wait: WaitRequest,
}

pub async fn start(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<StartRequest>,
) -> Response {
    let Some(tasks) = s.durable_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(tasks.start(&c, r.task).await, StatusCode::CREATED)
}

pub async fn get(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(tasks) = s.durable_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(tasks.get(&c, id).await, StatusCode::OK)
}

pub async fn wait(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<WaitBody>,
) -> Response {
    let Some(tasks) = s.durable_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(tasks.wait(&c, id, r.wait).await, StatusCode::OK)
}

pub async fn resume(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(tasks) = s.durable_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(tasks.resume(&c, id).await, StatusCode::OK)
}

pub async fn cancel(
    State(s): State<AppState>,
    h: HeaderMap,
    Path(id): Path<Uuid>,
    Json(r): Json<ContextRequest>,
) -> Response {
    let Some(tasks) = s.durable_tasks.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(c) = context(s.host_trust.as_deref(), &h, r.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    reply(tasks.cancel(&c, id).await, StatusCode::OK)
}

fn reply(
    r: Result<crate::durable_tasks::DurableTask, DurableTaskError>,
    ok: StatusCode,
) -> Response {
    match r {
        Ok(v) => (ok, Json(v)).into_response(),
        Err(DurableTaskError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(DurableTaskError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(DurableTaskError::Conflict) => StatusCode::CONFLICT.into_response(),
        Err(DurableTaskError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
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
