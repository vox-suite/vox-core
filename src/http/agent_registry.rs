use super::{AppState, auth};
use crate::agent_registry::{
    AgentRegistryError, RegisterAgentDefinitionRequest, SelectAgentRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

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
