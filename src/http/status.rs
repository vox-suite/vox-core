/**
* Status, ping, and observability health check HTTP endpoints.
*/
use super::{AppState, host_apps::assertion_from_headers};
use crate::{
    host_trust::{HostContextRequest, HostTrustService},
    status::{CreateSubscriptionRequest, StatusError},
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use chrono::Utc;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct ListRequest {
    pub host_context: HostContextRequest,
    pub after: Option<i64>,
    pub limit: Option<i64>,
}

#[derive(Deserialize)]
pub struct SubscriptionRequest {
    pub host_context: HostContextRequest,
    #[serde(flatten)]
    pub subscription: CreateSubscriptionRequest,
}

#[derive(Deserialize)]
pub struct SubscriptionActionRequest {
    pub host_context: HostContextRequest,
}

pub async fn list(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ListRequest>,
) -> Response {
    let Some(context) = authenticate(&state, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.status.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .list(
            &context,
            request.after.unwrap_or(0),
            request.limit.unwrap_or(50),
        )
        .await
    {
        Ok(events) => {
            let next_cursor = events
                .last()
                .map(|event| event.cursor)
                .unwrap_or(request.after.unwrap_or(0));
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "events": events,
                    "next_cursor": next_cursor,
                    "authoritative": false,
                    "fetch_authoritative_state": true,
                })),
            )
                .into_response()
        }
        Err(error) => reply(error),
    }
}

pub async fn create_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubscriptionRequest>,
) -> Response {
    let Some(context) = authenticate(&state, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.status.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service
        .create_subscription(&context, request.subscription)
        .await
    {
        Ok(subscription) => (StatusCode::CREATED, Json(subscription)).into_response(),
        Err(error) => reply(error),
    }
}

pub async fn list_subscriptions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<SubscriptionActionRequest>,
) -> Response {
    let Some(context) = authenticate(&state, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.status.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.list_subscriptions(&context).await {
        Ok(subscriptions) => (StatusCode::OK, Json(subscriptions)).into_response(),
        Err(error) => reply(error),
    }
}

pub async fn rotate_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SubscriptionActionRequest>,
) -> Response {
    let Some(context) = authenticate(&state, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.status.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.rotate_subscription(&context, id).await {
        Ok(subscription) => (StatusCode::OK, Json(subscription)).into_response(),
        Err(error) => reply(error),
    }
}

pub async fn disable_subscription(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<SubscriptionActionRequest>,
) -> Response {
    let Some(context) = authenticate(&state, &headers, request.host_context).await else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.status.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.disable_subscription(&context, id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => reply(error),
    }
}

fn reply(error: StatusError) -> Response {
    match error {
        StatusError::Invalid => StatusCode::BAD_REQUEST.into_response(),
        StatusError::NotFound => StatusCode::NOT_FOUND.into_response(),
        StatusError::Unavailable | StatusError::Database(_) | StatusError::Execution(_) => {
            StatusCode::SERVICE_UNAVAILABLE.into_response()
        }
    }
}

async fn authenticate(
    state: &AppState,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust: &HostTrustService = state.host_trust.as_deref()?;
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|value| value.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
        .ok()
}
