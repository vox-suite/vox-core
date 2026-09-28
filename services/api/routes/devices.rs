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
    domain::{devices::local_llm_capable, identity::Actor},
};

#[derive(Clone)]
pub struct DeviceApiState {
    pub devices: DeviceService,
}

pub async fn register_device(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<RegisterDeviceInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let device = state
        .devices
        .register_device(&actor, input)
        .await
        .map_err(|err| {
            tracing::warn!(user_id = %actor.user_id, %err, "Device registration failed");
            StatusCode::INTERNAL_SERVER_ERROR
        })?;
    tracing::info!(
        device_id = %device.id,
        user_id = %actor.user_id,
        platform = %device.platform,
        label = %device.label,
        local_llm_capable = local_llm_capable(&device.capabilities),
        "Device registered"
    );
    Ok((StatusCode::CREATED, Json(device)))
}

pub async fn heartbeat(
    State(state): State<DeviceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<impl IntoResponse, StatusCode> {
    let ok = state
        .devices
        .heartbeat(&actor, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if ok {
        Ok(StatusCode::OK)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}
