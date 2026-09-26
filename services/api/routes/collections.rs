/**
* HTTP handlers for managing dynamic record collections.
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
    application::collections::{
        CollectionService, CollectionServiceError, CreateCollectionInput, UpdateCollectionInput,
    },
    domain::identity::Actor,
};

#[derive(Deserialize)]
pub struct ListCollectionsQuery {
    pub limit: Option<i64>,
}

pub async fn list_collections(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<ListCollectionsQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let collections = service
        .list_collections(&actor, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(collections))
}

pub async fn create_collection(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateCollectionInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let collection = service
        .create_collection(&actor, input)
        .await
        .map_err(status_for)?;
    Ok((StatusCode::CREATED, Json(collection)))
}

pub async fn get_collection(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let collection = service
        .get_collection(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match collection {
        Some(c) => Ok(Json(c)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn archive_collection(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let archived = service
        .archive_collection(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if archived {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

fn status_for(err: CollectionServiceError) -> StatusCode {
    match err {
        CollectionServiceError::Invalid(_) => StatusCode::BAD_REQUEST,
        CollectionServiceError::NotFound => StatusCode::NOT_FOUND,
        CollectionServiceError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub async fn update_collection(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateCollectionInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let collection = service
        .update_collection(&actor, id, input)
        .await
        .map_err(status_for)?;
    Ok(Json(collection))
}

pub async fn add_collection_span(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Path((id, span_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, StatusCode> {
    service
        .add_span(&actor, id, span_id)
        .await
        .map_err(status_for)?;
    Ok(StatusCode::NO_CONTENT)
}

pub async fn remove_collection_span(
    State(service): State<CollectionService>,
    Extension(actor): Extension<Actor>,
    Path((id, span_id)): Path<(Uuid, Uuid)>,
) -> Result<impl IntoResponse, StatusCode> {
    let removed = service
        .remove_span(&actor, id, span_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if removed {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
