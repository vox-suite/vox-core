use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    connections::{AuthorizeConnectionRequest, ConnectionError},
    host_trust::{HostContextRequest, HostTrustError},
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
pub struct Request {
    pub host_context: HostContextRequest,
    pub authorization: AuthorizeConnectionRequest,
}

pub async fn authorize(
    State(s): State<AppState>,
    h: HeaderMap,
    Json(r): Json<Request>,
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
    match connections.record(&context, r.authorization).await {
        Ok(v) => (StatusCode::OK, Json(v)).into_response(),
        Err(ConnectionError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConnectionError::IntegrationUnavailable) => StatusCode::NOT_FOUND.into_response(),
        Err(ConnectionError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
