/**
* HTTP handlers for CRUD operations and querying JSONB records.
*/
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::Deserialize;
use uuid::Uuid;
use vox_core::{
    application::records::{
        CreateRecordInput, RecordService, RecordServiceError, UpdateRecordInput,
    },
    domain::identity::Actor,
};

#[derive(Deserialize)]
pub struct ListRecordsQuery {
    pub domain: Option<String>,
    pub limit: Option<i64>,
}

pub async fn list_records(
    State(service): State<RecordService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<ListRecordsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let records = service
        .list_records(&actor, query.domain.as_deref(), limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(records))
}

pub async fn create_record(
    State(service): State<RecordService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateRecordInput>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let record = service
        .create_record(&actor, input)
        .await
        .map_err(|e| match e {
            RecordServiceError::ValidationError(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            RecordServiceError::InvalidSchema(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            RecordServiceError::SchemaNotFound => {
                (StatusCode::BAD_REQUEST, "schema not found".to_string())
            }
            RecordServiceError::CollectionNotFound => {
                (StatusCode::BAD_REQUEST, "collection not found".to_string())
            }
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
        })?;
    Ok((StatusCode::CREATED, Json(record)))
}

pub async fn get_record(
    State(service): State<RecordService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let record = service
        .get_record(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match record {
        Some(r) => Ok(Json(r)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn update_record(
    State(service): State<RecordService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateRecordInput>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    let record = service
        .update_record(&actor, id, input)
        .await
        .map_err(|e| match e {
            RecordServiceError::ValidationError(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            RecordServiceError::InvalidSchema(msg) => (StatusCode::UNPROCESSABLE_ENTITY, msg),
            RecordServiceError::VersionConflict => {
                (StatusCode::CONFLICT, "version conflict".to_string())
            }
            RecordServiceError::NotFound => (StatusCode::NOT_FOUND, "record not found".to_string()),
            _ => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal server error".to_string(),
            ),
        })?;
    Ok(Json(record))
}

pub async fn delete_record(
    State(service): State<RecordService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let deleted = service
        .delete_record(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
