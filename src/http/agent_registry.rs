/**
* HTTP endpoints for agent registration and discovery.
*/
use super::{AppState, auth, host_apps::assertion_from_headers};
use crate::agent_registry::{
    AgentRegistryError, RegisterAgentDefinitionRequest, SelectAgentRequest, SetAgentEnabledRequest,
};
use crate::host_trust::HostContextRequest;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct HostSelectedRequest {
    pub host_context: HostContextRequest,
}

pub async fn list_selected_for_host(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HostSelectedRequest>,
) -> Response {
    let (Some(registry), Some(trust), Some(db)) = (
        state.agent_registry.as_ref(),
        state.host_trust.as_ref(),
        state.db.as_ref(),
    ) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(context) => context,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let deployment_key: Option<String> =
        sqlx::query_scalar("SELECT external_key FROM platform_deployments WHERE id=$1")
            .bind(context.subject.deployment_id.0)
            .fetch_optional(db.pool())
            .await
            .ok()
            .flatten();
    let Some(deployment_key) = deployment_key else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match registry.selected_for_deployment(&deployment_key).await {
        Ok(agents) => (StatusCode::OK, Json(agents)).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RegisterAgentDefinitionRequest>,
) -> Response {
    let Some(registry) = state.agent_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match registry.register(request).await {
        Ok(definition) => (StatusCode::CREATED, Json(definition)).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn select(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SelectAgentRequest>,
) -> Response {
    let Some(registry) = state.agent_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match registry.select(request).await {
        Ok(selected) => (StatusCode::CREATED, Json(selected)).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn list_selected(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(external_key): Path<String>,
) -> Response {
    let Some(registry) = state.agent_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match registry.selected_for_deployment(&external_key).await {
        Ok(selected) => (StatusCode::OK, Json(selected)).into_response(),
        Err(error) => registry_error(error),
    }
}

pub async fn set_enabled(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SetAgentEnabledRequest>,
) -> Response {
    let Some(registry) = state.agent_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match registry.set_enabled(request).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => registry_error(error),
    }
}

fn registry_error(error: AgentRegistryError) -> Response {
    match error {
        AgentRegistryError::InvalidDefinition
        | AgentRegistryError::InvalidModelConfiguration
        | AgentRegistryError::SensitiveModelConfiguration => {
            StatusCode::BAD_REQUEST.into_response()
        }
        AgentRegistryError::NotFound => StatusCode::NOT_FOUND.into_response(),
        AgentRegistryError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
