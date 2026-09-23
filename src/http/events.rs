/**
* HTTP endpoints for publishing and receiving domain events.
*/
use super::{AppState, auth};
use crate::events::{IngestEventRequest, service::EventError};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

pub async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<IngestEventRequest>,
) -> Response {
    let Some(service) = state.events.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.ingest(request).await {
        Ok(response) => (StatusCode::ACCEPTED, Json(response)).into_response(),
        Err(EventError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(EventError::Database(_) | EventError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
