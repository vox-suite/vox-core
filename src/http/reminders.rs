/**
 * HTTP endpoints for explicit-timezone reminders and delivery status (E40).
 */
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::HostContextRequest,
    reminders::{
        CreateReminderRequest, RecordDeliveryRequest, ReminderDeliveryStatus, ReminderError,
        ReminderScheduleKind,
    },
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct CreateReminderApiRequest {
    pub host_context: HostContextRequest,
    pub title: String,
    pub message: String,
    pub channel: String,
    pub destination: String,
    pub timezone: String,
    pub schedule_kind: ReminderScheduleKind,
    pub run_at: Option<DateTime<Utc>>,
    pub interval_seconds: Option<i64>,
    pub recurrence_expression: Option<String>,
    pub max_retries: Option<i32>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
pub struct ContextOnlyApiRequest {
    pub host_context: HostContextRequest,
}

pub async fn create_reminder(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateReminderApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let domain_req = CreateReminderRequest {
        title: request.title,
        message: request.message,
        channel: request.channel,
        destination: request.destination,
        timezone: request.timezone,
        schedule_kind: request.schedule_kind,
        run_at: request.run_at,
        interval_seconds: request.interval_seconds,
        recurrence_expression: request.recurrence_expression,
        max_retries: request.max_retries,
        metadata: request.metadata,
    };

    match service.create(&context, domain_req, Utc::now()).await {
        Ok(reminder) => (StatusCode::CREATED, Json(reminder)).into_response(),
        Err(ReminderError::Invalid(err)) => (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": err }))).into_response(),
        Err(ReminderError::ActionAuthorityProhibited) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Reminders cannot authorize actions or execute consequential writes" })),
        ).into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(ReminderError::Unauthorized) => StatusCode::UNAUTHORIZED.into_response(),
        Err(ReminderError::NotFound) => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn list_reminders(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ContextOnlyApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    match service.list(&context).await {
        Ok(reminders) => (StatusCode::OK, Json(reminders)).into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn get_reminder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<ContextOnlyApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    match service.get(&context, id).await {
        Ok(reminder) => (StatusCode::OK, Json(reminder)).into_response(),
        Err(ReminderError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn cancel_reminder(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<ContextOnlyApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    match service.cancel(&context, id).await {
        Ok(reminder) => (StatusCode::OK, Json(reminder)).into_response(),
        Err(ReminderError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn get_reminder_deliveries(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<ContextOnlyApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    match service.get_deliveries(&context, id).await {
        Ok(deliveries) => (StatusCode::OK, Json(deliveries)).into_response(),
        Err(ReminderError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

#[derive(Deserialize)]
pub struct RecordDeliveryApiRequest {
    pub host_context: HostContextRequest,
    pub status: ReminderDeliveryStatus,
    pub channel: String,
    pub destination: String,
    pub provider_receipt_id: Option<String>,
    pub failure_reason: Option<String>,
    pub attempted_at: Option<DateTime<Utc>>,
}

pub async fn record_reminder_delivery(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(request): Json<RecordDeliveryApiRequest>,
) -> Response {
    let Some(service) = state.reminders.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(trust) = state.host_trust.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let assertion = match assertion_from_headers(&headers) {
        Ok(a) => a,
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    let context = match trust
        .resolve_authenticated_context(&assertion, &request.host_context, origin, Utc::now())
        .await
    {
        Ok(c) => c,
        Err(crate::host_trust::HostTrustError::InvalidRequest) => {
            return StatusCode::BAD_REQUEST.into_response();
        }
        Err(
            crate::host_trust::HostTrustError::Database(_)
            | crate::host_trust::HostTrustError::Identity(_),
        ) => {
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(_) => return StatusCode::UNAUTHORIZED.into_response(),
    };

    let attempted_at = request.attempted_at.unwrap_or_else(Utc::now);

    let domain_req = RecordDeliveryRequest {
        reminder_id: id,
        status: request.status,
        channel: request.channel,
        destination: request.destination,
        provider_receipt_id: request.provider_receipt_id,
        failure_reason: request.failure_reason,
        attempted_at,
    };

    match service.record_delivery(&context, domain_req).await {
        Ok(delivery) => (StatusCode::CREATED, Json(delivery)).into_response(),
        Err(ReminderError::NotFound) => StatusCode::NOT_FOUND.into_response(),
        Err(ReminderError::Database(_)) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
