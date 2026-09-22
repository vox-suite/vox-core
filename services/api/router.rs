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
        collections::{archive_collection, create_collection, get_collection, list_collections},
        devices::{heartbeat, register_device},
        identity::get_me,
        records::{create_record, delete_record, get_record, list_records},
        schemas::{create_schema_version, get_schema_by_name},
        tasks::{create_task, delete_task, get_task, list_tasks},
    },
    state::ApiState,
};

pub fn build_api_router(state: ApiState) -> Router {
    let task_routes = Router::new()
        .route("/v1/tasks", get(list_tasks).post(create_task))
        .route("/v1/tasks/{id}", get(get_task).delete(delete_task))
        .with_state(state.tasks.clone());

    let collection_routes = Router::new()
        .route("/v1/collections", get(list_collections).post(create_collection))
        .route("/v1/collections/{id}", get(get_collection).delete(archive_collection))
        .with_state(state.collections.clone());

    let record_routes = Router::new()
        .route("/v1/records", get(list_records).post(create_record))
        .route("/v1/records/{id}", get(get_record).delete(delete_record))
        .with_state(state.records.clone());

    let schema_routes = Router::new()
        .route("/v1/schemas", post(create_schema_version))
        .route("/v1/schemas/{namespace}/{name}", get(get_schema_by_name))
        .with_state(state.schemas.clone());

    let device_routes = Router::new()
        .route("/v1/devices", post(register_device))
        .route("/v1/devices/{id}/heartbeat", post(heartbeat))
        .with_state(state.devices.clone());

    let identity_routes = Router::new().route("/v1/me", get(get_me));

    let openapi_route = Router::new().route("/openapi.json", get(get_openapi_spec));

    let base_legacy_router = vox_core::http::router(state.legacy);

    base_legacy_router
        .merge(openapi_route)
        .merge(
            task_routes
                .merge(collection_routes)
                .merge(record_routes)
                .merge(schema_routes)
                .merge(device_routes)
                .merge(identity_routes)
                .layer(middleware::from_fn(extract_actor)),
        )
}
