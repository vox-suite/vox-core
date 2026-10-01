/**
* HTTP handlers for schema registration and validation lookup.
*/
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use vox_core::{
    application::schemas::{CreateSchemaVersionInput, SchemaService},
    domain::identity::Actor,
};

#[utoipa::path(
    post,
    path = "/v1/schemas",
    tag = "schemas",
    request_body = vox_core::application::schemas::CreateSchemaVersionInput,
    responses((status = 201, body = vox_core::domain::schemas::DataSchema))
)]
pub async fn create_schema_version(
    State(service): State<SchemaService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateSchemaVersionInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let schema = service
        .create_version(&actor, input)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(schema)))
}

#[utoipa::path(
    post,
    path = "/v1/schemas/{namespace}/{name}",
    tag = "schemas",
    params(("namespace" = String, Path), ("name" = String, Path)),
    responses((status = 200, body = vox_core::domain::schemas::DataSchema))
)]
pub async fn get_schema_by_name(
    State(service): State<SchemaService>,
    Extension(actor): Extension<Actor>,
    Path((namespace, name)): Path<(String, String)>,
) -> Result<impl IntoResponse, StatusCode> {
    let schema = service
        .get_by_name(&actor, &namespace, &name)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    match schema {
        Some(s) => Ok(Json(s)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[utoipa::path(
    get,
    path = "/v1/me/schemas",
    tag = "schemas",
    responses((status = 200, body = Vec<vox_core::domain::schemas::DataSchema>))
)]
pub async fn list_schemas(
    State(service): State<SchemaService>,
    Extension(actor): Extension<Actor>,
) -> Result<impl IntoResponse, StatusCode> {
    let schemas = service
        .list_for_user(&actor)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(schemas))
}
