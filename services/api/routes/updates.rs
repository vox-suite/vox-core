use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;
use vox_core::{
    application::updates::{UpdatesService, UpdatesServiceError},
    domain::{
        identity::Actor,
        updates::{JobActionResponse, JobInputRequest, JobRetryRequest, UpdateItem, UpdatesQuery},
    },
    storage::updates::UpdatesStorageError,
};

fn status_for(err: UpdatesServiceError) -> StatusCode {
    match err {
        UpdatesServiceError::Unauthorized => StatusCode::FORBIDDEN,
        UpdatesServiceError::NotFound => StatusCode::NOT_FOUND,
        UpdatesServiceError::Invalid(_) => StatusCode::BAD_REQUEST,
        UpdatesServiceError::Storage(storage_err) => match storage_err {
            UpdatesStorageError::NotFound | UpdatesStorageError::JobNotFound => {
                StatusCode::NOT_FOUND
            }
            UpdatesStorageError::BoundedAttemptsExceeded => StatusCode::TOO_MANY_REQUESTS,
            UpdatesStorageError::InvalidJobState | UpdatesStorageError::InvalidInput(_) => {
                StatusCode::BAD_REQUEST
            }
            UpdatesStorageError::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        },
    }
}

#[utoipa::path(
    post,
    path = "/v1/updates/list",
    tag = "updates",
    request_body = UpdatesQuery,
    responses((status = 200, body = Vec<UpdateItem>))
)]
pub async fn list_updates(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Json(query): Json<UpdatesQuery>,
) -> Result<impl IntoResponse, StatusCode> {
    let items = service
        .list_updates(&actor, query)
        .await
        .map_err(status_for)?;
    Ok(Json(items))
}

#[utoipa::path(
    get,
    path = "/v1/updates/{id}",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Update id")),
    responses((status = 200, body = UpdateItem))
)]
pub async fn get_update(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let item = service.get_update(&actor, id).await.map_err(status_for)?;
    Ok(Json(item))
}

#[utoipa::path(
    post,
    path = "/v1/updates/{id}/read",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Update id")),
    responses((status = 200, body = UpdateItem))
)]
pub async fn mark_update_read(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let item = service.mark_read(&actor, id).await.map_err(status_for)?;
    Ok(Json(item))
}

#[utoipa::path(
    post,
    path = "/v1/updates/{id}/dismiss",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Update id")),
    responses((status = 200, body = UpdateItem))
)]
pub async fn dismiss_update(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let item = service.dismiss(&actor, id).await.map_err(status_for)?;
    Ok(Json(item))
}

#[utoipa::path(
    post,
    path = "/v1/updates/{id}/resolve",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Update id")),
    responses((status = 200, body = UpdateItem))
)]
pub async fn resolve_update(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let item = service.resolve(&actor, id).await.map_err(status_for)?;
    Ok(Json(item))
}

#[utoipa::path(
    post,
    path = "/v1/updates/jobs/{id}/retry",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Job id")),
    request_body = JobRetryRequest,
    responses((status = 200, body = JobActionResponse))
)]
pub async fn retry_job(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(req): Json<JobRetryRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let response = service
        .retry_job(&actor, id, req)
        .await
        .map_err(status_for)?;
    Ok(Json(response))
}

#[utoipa::path(
    post,
    path = "/v1/updates/jobs/{id}/input",
    tag = "updates",
    params(("id" = Uuid, Path, description = "Job id")),
    request_body = JobInputRequest,
    responses((status = 200, body = JobActionResponse))
)]
pub async fn provide_job_input(
    State(service): State<UpdatesService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(req): Json<JobInputRequest>,
) -> Result<impl IntoResponse, StatusCode> {
    let response = service
        .provide_job_input(&actor, id, req)
        .await
        .map_err(status_for)?;
    Ok(Json(response))
}
