use super::{AppState, host_apps::authenticated_context};
use crate::events::{IngestEventRequest, service::EventError};
use crate::host_trust::HostContextRequest;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};

#[derive(serde::Deserialize)]
pub struct AuthenticatedIngestEventRequest {
    pub host_context: Option<HostContextRequest>,
    #[serde(flatten)]
    pub event: IngestEventRequest,
}

pub async fn ingest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedIngestEventRequest>,
) -> Response {
    let Some(service) = state.events.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = authenticated_context(
        state.host_trust.as_deref(),
        &headers,
        request.host_context.as_ref(),
    )
    .await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.ingest(context.owner(), request.event).await {
        Ok(response) => (StatusCode::ACCEPTED, Json(response)).into_response(),
        Err(EventError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(EventError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
