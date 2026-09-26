//! Host-scoped declarative skill catalog and installation.
use super::{AppState, auth, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustError},
    identity::ResolvedUserContext,
    skills::{PublishSkillRequest, SkillError},
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
pub struct PrivatePublishRequest {
    pub host_context: HostContextRequest,
    pub skill: PublishSkillRequest,
}

#[derive(Deserialize)]
pub struct ContextRequest {
    pub host_context: HostContextRequest,
}

#[derive(Deserialize)]
pub struct CuratedPublishRequest {
    pub deployment_external_key: String,
    pub skill: PublishSkillRequest,
}

pub async fn publish_private(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PrivatePublishRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.publish_private(&context, request.skill).await {
        Ok(skill) => (StatusCode::CREATED, Json(skill)).into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn publish_curated(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CuratedPublishRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service
        .publish_curated(&request.deployment_external_key, request.skill)
        .await
    {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({"id": id}))).into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContextRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.list(&context).await {
        Ok(skills) => (StatusCode::OK, Json(skills)).into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn version(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((id, version)): Path<(Uuid, i32)>,
    Json(request): Json<ContextRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.version(&context, id, version).await {
        Ok(skill) => (StatusCode::OK, Json(skill)).into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn install(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<InstallRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.install(&context, id, request.version).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => reply_error(error),
    }
}

#[derive(Deserialize)]
pub struct InstallRequest {
    pub host_context: HostContextRequest,
    pub version: i32,
}

pub async fn disable(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ContextRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.disable(&context, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => reply_error(error),
    }
}

#[derive(Deserialize)]
pub struct AgentEnableRequest {
    pub host_context: HostContextRequest,
    pub enabled: bool,
}

pub async fn set_agent_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((agent_key, skill_id)): Path<(String, Uuid)>,
    Json(request): Json<AgentEnableRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service
        .set_agent_enabled(&context, &agent_key, skill_id, request.enabled)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn effective(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(agent_key): Path<String>,
    Json(request): Json<ContextRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.effective(&context, &agent_key).await {
        Ok(skills) => (StatusCode::OK, Json(skills)).into_response(),
        Err(error) => reply_error(error),
    }
}

pub async fn load_for_agent(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((agent_key, skill_id)): Path<(String, Uuid)>,
    Json(request): Json<ContextRequest>,
) -> Response {
    let Some(service) = state.skills.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let context = match context(&state, &headers, &request.host_context).await {
        Ok(context) => context,
        Err(status) => return status.into_response(),
    };
    match service.load_for_agent(&context, &agent_key, skill_id).await {
        Ok(skill) => (StatusCode::OK, Json(skill)).into_response(),
        Err(error) => reply_error(error),
    }
}

async fn context(
    state: &AppState,
    headers: &HeaderMap,
    request: &HostContextRequest,
) -> Result<ResolvedUserContext, StatusCode> {
    let Some(trust) = state.host_trust.as_ref() else {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let assertion = assertion_from_headers(headers).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, request, origin, Utc::now())
        .await
        .map_err(|error| match error {
            HostTrustError::InvalidRequest => StatusCode::BAD_REQUEST,
            HostTrustError::Database(_) | HostTrustError::Identity(_) => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            _ => StatusCode::UNAUTHORIZED,
        })
}

fn reply_error(error: SkillError) -> Response {
    match error {
        SkillError::Invalid => StatusCode::BAD_REQUEST.into_response(),
        SkillError::Conflict => StatusCode::CONFLICT.into_response(),
        SkillError::NotFound => StatusCode::NOT_FOUND.into_response(),
        SkillError::Database(_) | SkillError::Grant(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
