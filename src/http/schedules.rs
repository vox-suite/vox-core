/**
* HTTP endpoints for managing recurring schedules and cron triggers.
*/
use super::{AppState, auth};
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

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateScheduleRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.create(request).await {
        Ok(schedule) => (StatusCode::CREATED, Json(schedule)).into_response(),
        Err(ScheduleError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ScheduleError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ScheduleError::Database(_) | ScheduleError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

pub async fn update(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<UpdateScheduleRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    if !auth::authorized(&headers, &state.service_token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match service.update(ScheduleId(id), request).await {
        Ok(schedule) => (StatusCode::OK, Json(schedule)).into_response(),
        Err(ScheduleError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ScheduleError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ScheduleError::Database(_) | ScheduleError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
