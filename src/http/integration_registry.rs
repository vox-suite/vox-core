use super::{AppState, auth, host_apps::assertion_from_headers};
use crate::host_trust::{HostContextRequest, HostTrustError};
use crate::integration_registry::{
    IntegrationRegistryError, RegisterIntegrationRequest, SetIntegrationEnabledRequest,
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
pub struct ContextDiscoveryRequest {
    pub host_context: HostContextRequest,
    pub agent_external_key: Option<String>,
    pub region: Option<String>,
}
pub async fn register(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<RegisterIntegrationRequest>,
) -> Response {
    let Some(x) = s.integration_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match x.register(r).await {
        Ok(()) => StatusCode::CREATED.into_response(),
        Err(e) => err(e),
    }
}

pub async fn set_enabled(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<SetIntegrationEnabledRequest>,
) -> Response {
    let Some(x) = s.integration_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match x.set_enabled(r).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => err(e),
    }
}

pub async fn discover(State(s): State<AppState>, h: HeaderMap, Path(k): Path<String>) -> Response {
    let Some(x) = s.integration_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match x.discover(&k).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(e) => err(e),
    }
}

pub async fn versions(
    State(s): State<AppState>,
    h: HeaderMap,
    Path((deployment_key, integration_key)): Path<(String, String)>,
) -> Response {
    let Some(registry) = s.integration_registry.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&h, &s.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match registry.versions(&deployment_key, &integration_key).await {
        Ok(versions) => (StatusCode::OK, Json(versions)).into_response(),
        Err(error) => err(error),
    }
}

pub async fn discover_for_context(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<ContextDiscoveryRequest>,
) -> Response {
    let (Some(registry), Some(trust)) = (s.integration_registry.as_ref(), s.host_trust.as_ref())
    else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(assertion) = assertion_from_headers(&h) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let origin = h.get("origin").and_then(|value| value.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &r.host_context, origin, Utc::now())
        .await
    {
        Ok(context) => context,
        Err(HostTrustError::InvalidRequest) => return StatusCode::BAD_REQUEST.into_response(),
        Err(HostTrustError::Database(_) | HostTrustError::Identity(_)) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    match registry
        .discover_for_context(
            &context.request_context(),
            r.agent_external_key.as_deref(),
            r.region.as_deref(),
        )
        .await
    {
        Ok(capabilities) => (StatusCode::OK, Json(capabilities)).into_response(),
        Err(error) => err(error),
    }
}

fn err(e: IntegrationRegistryError) -> Response {
    match e {
        IntegrationRegistryError::Invalid => StatusCode::BAD_REQUEST.into_response(),
        IntegrationRegistryError::NotFound => StatusCode::NOT_FOUND.into_response(),
        IntegrationRegistryError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
