/**
* HTTP handlers for creating and inspecting background tasks.
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
    application::tasks::{CreateTaskInput, TaskService, TaskServiceError, UpdateTaskInput},
    domain::identity::Actor,
};

#[derive(Deserialize)]
pub struct ListTasksQuery {
    pub limit: Option<i64>,
}

pub async fn list_tasks(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<ListTasksQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let limit = query.limit.unwrap_or(50).clamp(1, 100);
    let tasks = service
        .list_tasks(&actor, limit)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(tasks))
}

pub async fn create_task(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateTaskInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let task = service
        .create_task(&actor, input)
        .await
        .map_err(|e| match e {
            TaskServiceError::CollectionNotFound => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    Ok((StatusCode::CREATED, Json(task)))
}

pub async fn get_task(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let task = service
        .get_task(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match task {
        Some(t) => Ok(Json(t)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

pub async fn update_task(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateTaskInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let task = service
        .update_task(&actor, id, input)
        .await
        .map_err(|e| match e {
            TaskServiceError::NotFound => StatusCode::NOT_FOUND,
            TaskServiceError::VersionConflict => StatusCode::CONFLICT,
            TaskServiceError::CollectionNotFound => StatusCode::BAD_REQUEST,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        })?;
    Ok(Json(task))
}

pub async fn delete_task(
    State(service): State<TaskService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let deleted = service
        .delete_task(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if deleted {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
