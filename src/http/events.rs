/**
* HTTP endpoints for publishing and receiving domain events.
*/
use super::{AppState, context::context};
use crate::events::{IngestEventRequest, service::EventError};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

#[derive(serde::Deserialize)]
pub struct HostIngestEventRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub request: IngestEventRequest,
}

pub async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HostIngestEventRequest>,
) -> Response {
    let Some(service) = state.events.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.ingest(context, request.request).await {
        Ok(response) => (StatusCode::ACCEPTED, Json(response)).into_response(),
        Err(EventError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(EventError::Database(_) | EventError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
