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
