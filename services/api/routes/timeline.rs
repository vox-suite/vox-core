use axum::{
    Extension, Json,
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use uuid::Uuid;
use vox_core::{
    application::timeline::{TimelineService, TimelineServiceError},
    domain::{
        identity::Actor,
        timeline::{
            IngestTimelineEventInput, NewEventType, TimelineEventType, TimelineEventWithEvidence,
            TimelineGroup, TimelinePage, TimelineQuery,
        },
    },
    storage::timeline::TimelineStorageError,
};

#[derive(Debug, Deserialize)]
pub struct EventTypeFilterParams {
    pub group_id: Option<Uuid>,
    pub group: Option<String>,
}

fn status_for(err: TimelineServiceError) -> StatusCode {
    match err {
        TimelineServiceError::Unauthorized => StatusCode::FORBIDDEN,
        TimelineServiceError::NotFound(_) => StatusCode::NOT_FOUND,
        TimelineServiceError::Invalid(_) => StatusCode::BAD_REQUEST,
        TimelineServiceError::Storage(storage_err) => match storage_err {
            TimelineStorageError::EventTypeNotFound | TimelineStorageError::GroupNotFound => {
                StatusCode::NOT_FOUND
            }
            TimelineStorageError::GroupMismatch
            | TimelineStorageError::InvalidJsonSchema(_)
            | TimelineStorageError::ValidationFailed(_)
            | TimelineStorageError::InvalidCursor => StatusCode::BAD_REQUEST,
            TimelineStorageError::UnauthorizedEventType => StatusCode::FORBIDDEN,
            TimelineStorageError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

#[utoipa::path(
    get,
    path = "/v1/timeline/groups",
    tag = "timeline",
    responses((status = 200, body = Vec<TimelineGroup>))
)]
pub async fn list_groups(
    State(service): State<TimelineService>,
) -> Result<impl IntoResponse, StatusCode> {
    let groups = service.list_groups().await.map_err(status_for)?;
    Ok(Json(groups))
}

#[utoipa::path(
    get,
    path = "/v1/timeline/event-types",
    tag = "timeline",
    params(
        ("group_id" = Option<Uuid>, Query, description = "Filter by group id"),
        ("group" = Option<String>, Query, description = "Filter by group string value")
    ),
    responses((status = 200, body = Vec<TimelineEventType>))
)]
pub async fn list_event_types(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Query(params): Query<EventTypeFilterParams>,
) -> Result<impl IntoResponse, StatusCode> {
    let types = service
        .list_event_types(&actor, params.group_id, params.group.as_deref())
        .await
        .map_err(status_for)?;
    Ok(Json(types))
}

#[utoipa::path(
    post,
    path = "/v1/timeline/event-types",
    tag = "timeline",
    request_body = NewEventType,
    responses((status = 201, body = TimelineEventType))
)]
pub async fn create_event_type(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<NewEventType>,
) -> Result<impl IntoResponse, StatusCode> {
    let created = service
        .create_event_type(&actor, input)
        .await
        .map_err(status_for)?;
    Ok((StatusCode::CREATED, Json(created)))
}

#[utoipa::path(
    post,
    path = "/v1/timeline/events/query",
    tag = "timeline",
    request_body = TimelineQuery,
    responses((status = 200, body = TimelinePage))
)]
pub async fn query_events(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Json(query): Json<TimelineQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let page = service
        .query_events(&actor, query)
        .await
        .map_err(status_for)?;
    Ok(Json(page))
}

#[utoipa::path(
    post,
    path = "/v1/timeline/events",
    tag = "timeline",
    request_body = IngestTimelineEventInput,
    responses((status = 201, body = TimelineEventWithEvidence))
)]
pub async fn ingest_event(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<IngestTimelineEventInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let item = service
        .ingest_event(&actor, input)
        .await
        .map_err(status_for)?;
    Ok((StatusCode::CREATED, Json(item)))
}

#[utoipa::path(post, path="/v1/timeline/events/counts", tag="timeline",
    request_body=vox_core::domain::timeline::TimelineCountsQuery,
    responses((status=200, body=Vec<vox_core::domain::timeline::TimelineDayCount>)))]
pub async fn day_counts(
    State(service): State<TimelineService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<vox_core::domain::timeline::TimelineCountsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let counts = service
        .repo()
        .day_counts(actor.user_id, input)
        .await
        .map_err(|e| status_for(e.into()))?;
    Ok(Json(counts))
}
