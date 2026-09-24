/**
* Route definitions and Axum state assembly for the public HTTP API.
*/
use axum::{
    Router, middleware,
    routing::{get, post},
};

use crate::{
    auth::extract_actor,
    openapi::get_openapi_spec,
    routes::{
        auth::exchange_token,
        collections::{archive_collection, create_collection, get_collection, list_collections},
        device_socket::{DeviceSocketState, device_socket},
        devices::{
            DeviceApiState, claim_device_jobs, heartbeat, register_device, submit_job_result,
        },
        events::ingest_batch,
        identity::get_me,
        phone::link_phone,
        records::{create_record, delete_record, get_record, list_records, update_record},
        schemas::{create_schema_version, get_schema_by_name},
        tasks::{create_task, delete_task, get_task, list_tasks, update_task},
    },
    state::ApiState,
};

pub fn build_api_router(state: ApiState) -> Router {
    let auth_routes = Router::new()
        .route("/v1/auth/exchange", post(exchange_token))
        .with_state(state.pool.clone());

    let task_routes = Router::new()
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route(
            "/v1/tasks/{id}",
            get(get_task).patch(update_task).delete(delete_task),
        )
        .with_state(state.tasks.clone());

    let collection_routes = Router::new()
        .route(
            "/v1/collections",
            get(list_collections).post(create_collection),
        )
        .route(
            "/v1/collections/{id}",
            get(get_collection).delete(archive_collection),
        )
        .with_state(state.collections.clone());

    let record_routes = Router::new()
        .route("/v1/records", get(list_records).post(create_record))
        .route(
            "/v1/records/{id}",
            get(get_record).patch(update_record).delete(delete_record),
        )
        .with_state(state.records.clone());

    let schema_routes = Router::new()
        .route("/v1/schemas", post(create_schema_version))
        .route("/v1/schemas/{namespace}/{name}", get(get_schema_by_name))
        .with_state(state.schemas.clone());

    let device_api_state = DeviceApiState {
        devices: state.devices.clone(),
        pool: state.pool.clone(),
    };
    let device_routes = Router::new()
        .route("/v1/devices", post(register_device))
        .route("/v1/devices/{id}/heartbeat", post(heartbeat))
        .route("/v1/devices/{id}/jobs/claim", post(claim_device_jobs))
        .route("/v1/device-jobs/{id}/result", post(submit_job_result))
        .with_state(device_api_state);

    let device_socket_state = DeviceSocketState {
        hub: state.device_hub.clone(),
        pool: state.pool.clone(),
    };
    let device_socket_routes = Router::new()
        .route("/v1/devices/{id}/socket", get(device_socket))
        .with_state(device_socket_state);

    let event_routes = Router::new()
        .route("/v1/events/batch", post(ingest_batch))
        .with_state(state.pool.clone());

    let identity_routes = Router::new()
        .route("/v1/me", get(get_me))
        .with_state(state.pool.clone());

    let phone_routes = Router::new()
        .route("/v1/me/phone", post(link_phone))
        .with_state(state.pool.clone());

    let openapi_route = Router::new().route("/openapi.json", get(get_openapi_spec));

    let protected_routes = task_routes
        .merge(collection_routes)
        .merge(record_routes)
        .merge(schema_routes)
        .merge(device_routes)
        .merge(device_socket_routes)
        .merge(event_routes)
        .merge(identity_routes)
        .merge(phone_routes)
        .layer(middleware::from_fn_with_state(
            state.pool.clone(),
            extract_actor,
        ));

    let base_legacy_router = vox_core::http::router(state.legacy);

    base_legacy_router
        .merge(openapi_route)
        .merge(auth_routes)
        .merge(protected_routes)
}
