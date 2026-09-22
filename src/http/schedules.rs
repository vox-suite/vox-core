use super::{AppState, host_apps::authenticated_context};
use crate::host_trust::HostContextRequest;
use crate::schedules::{
    CreateScheduleRequest, ScheduleId, UpdateScheduleRequest, service::ScheduleError,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use uuid::Uuid;

#[derive(serde::Deserialize)]
pub struct CreateRequest {
    pub host_context: Option<HostContextRequest>,
    #[serde(flatten)]
    pub schedule: CreateScheduleRequest,
}

#[derive(serde::Deserialize)]
pub struct UpdateRequest {
    pub host_context: Option<HostContextRequest>,
    #[serde(flatten)]
    pub schedule: UpdateScheduleRequest,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
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
    match service.create(context.owner(), request.schedule).await {
        Ok(schedule) => (StatusCode::CREATED, Json(schedule)).into_response(),
        Err(ScheduleError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ScheduleError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ScheduleError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<UpdateRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
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
    match service
        .update(context.owner(), ScheduleId(id), request.schedule)
        .await
    {
        Ok(schedule) => (StatusCode::OK, Json(schedule)).into_response(),
        Err(ScheduleError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ScheduleError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ScheduleError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}
