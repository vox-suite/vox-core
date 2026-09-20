use super::{AppState, auth};
use crate::integration_registry::{
    IntegrationRegistryError, RegisterIntegrationRequest, SetIntegrationEnabledRequest,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
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
fn err(e: IntegrationRegistryError) -> Response {
    match e {
        IntegrationRegistryError::Invalid => StatusCode::BAD_REQUEST.into_response(),
        IntegrationRegistryError::NotFound => StatusCode::NOT_FOUND.into_response(),
        IntegrationRegistryError::Database(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
