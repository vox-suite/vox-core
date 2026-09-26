/**
* HTTP handlers for spans: the unified timeline of past, present, and planned activity.
*/
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;
use vox_core::{
    application::spans::{SpanService, SpanServiceError},
    domain::{
        identity::Actor,
        spans::{NewSpan, SpanPatch, SpanQuery},
    },
};

fn status_for(err: SpanServiceError) -> StatusCode {
    match err {
        SpanServiceError::Invalid(_) | SpanServiceError::ReferenceNotFound => {
            StatusCode::BAD_REQUEST
        }
        SpanServiceError::NotFound => StatusCode::NOT_FOUND,
        SpanServiceError::VersionConflict => StatusCode::CONFLICT,
        SpanServiceError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub async fn list_spans(
    State(service): State<SpanService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<SpanQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let spans = service
        .list_spans(&actor, &query)
        .await
        .map_err(status_for)?;
    Ok(Json(spans))
}

pub async fn create_span(
    State(service): State<SpanService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<NewSpan>,
) -> Result<impl IntoResponse, StatusCode> {
    let span = service
        .create_span(&actor, input)
        .await
        .map_err(status_for)?;
    Ok((StatusCode::CREATED, Json(span)))
}

pub async fn get_span(
    State(service): State<SpanService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    service
        .get_span(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}

pub async fn update_span(
    State(service): State<SpanService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(patch): Json<SpanPatch>,
) -> Result<impl IntoResponse, StatusCode> {
    let span = service
        .update_span(&actor, id, patch)
        .await
        .map_err(status_for)?;
    Ok(Json(span))
}

pub async fn delete_span(
    State(service): State<SpanService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let deleted = service
        .delete_span(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
