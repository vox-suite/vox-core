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
    application::collections::{CollectionService, CreateCollectionInput},
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
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
