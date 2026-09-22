/**
* HTTP handlers for device pairing, tokens, and registration.
*/
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use uuid::Uuid;
use vox_core::{
    application::devices::{DeviceService, RegisterDeviceInput},
    domain::identity::Actor,
};

pub async fn register_device(
    State(service): State<DeviceService>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<RegisterDeviceInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let device = service
        .register_device(&actor, input)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok((StatusCode::CREATED, Json(device)))
}

pub async fn heartbeat(
    State(service): State<DeviceService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let ok = service
        .heartbeat(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if ok {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
