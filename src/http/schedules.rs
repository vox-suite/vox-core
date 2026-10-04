/**
* HTTP endpoints for managing recurring schedules and cron triggers.
*/
use super::{AppState, context::context};
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
pub struct HostCreateScheduleRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub request: CreateScheduleRequest,
}

#[derive(serde::Deserialize)]
pub struct HostUpdateScheduleRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub request: UpdateScheduleRequest,
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<HostCreateScheduleRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service.create(context, request.request).await {
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
    Json(request): Json<HostUpdateScheduleRequest>,
) -> Response {
    let Some(service) = state.schedules.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match service
        .update(ScheduleId(id), context, request.request)
        .await
    {
        Ok(schedule) => (StatusCode::OK, Json(schedule)).into_response(),
        Err(ScheduleError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ScheduleError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ScheduleError::Database(_) | ScheduleError::Identity(_)) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}
