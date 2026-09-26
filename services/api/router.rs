/**
* Route definitions and Axum state assembly for the public HTTP API.
*/
use axum::{
    Router, middleware,
    routing::{get, post, put},
};

use crate::{
    auth::extract_actor,
    openapi::get_openapi_spec,
    routes::{
        auth::exchange_token,
        collections::{
            add_collection_span, archive_collection, create_collection, get_collection,
            list_collections, remove_collection_span, update_collection,
        },
        device_socket::{DeviceSocketState, device_socket},
        devices::{
            DeviceApiState, claim_device_jobs, heartbeat, register_device, submit_job_result,
        },
        events::ingest_batch,
        identity::get_me,
        internal::dispatch_device_request,
        live::{LiveApiState, live_socket},
        location::{
            get_consent as get_location_consent, grant_consent as grant_location_consent,
            revoke_consent as revoke_location_consent, submit_segments,
        },
        phone::{PhoneApiState, link_phone},
        records::{create_record, delete_record, get_record, list_records, update_record},
        schemas::{create_schema_version, get_schema_by_name},
        sms::{get_consent, grant_consent, revoke_consent, submit_batch},
        spans::{create_span, delete_span, get_span, list_spans, update_span},
    },
    state::ApiState,
};

pub fn build_api_router(state: ApiState) -> Router {
    let auth_routes = Router::new()
        .route("/v1/auth/exchange", post(exchange_token))
        .with_state(state.pool.clone());

    let span_routes = Router::new()
        .route("/v1/spans", get(list_spans).post(create_span))
        .route(
            "/v1/spans/{id}",
            get(get_span).patch(update_span).delete(delete_span),
        )
        .with_state(state.spans.clone());

    let collection_routes = Router::new()
        .route(
            "/v1/collections",
            get(list_collections).post(create_collection),
        )
        .route(
            "/v1/collections/{id}",
            get(get_collection)
                .patch(update_collection)
                .delete(archive_collection),
        )
        .route(
            "/v1/collections/{id}/spans/{span_id}",
            put(add_collection_span).delete(remove_collection_span),
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

    let sms_routes = Router::new()
        .route("/v1/sms/batches", post(submit_batch))
        .with_state(state.sms_ingestion.clone());

    let sms_consent_routes = Router::new()
        .route(
            "/v1/sms/consent",
            get(get_consent).post(grant_consent).delete(revoke_consent),
        )
        .with_state(state.consent.clone());

    let location_routes = Router::new()
        .route("/v1/location/segments", post(submit_segments))
        .with_state(state.location_ingestion.clone());

    let location_consent_routes = Router::new()
        .route(
            "/v1/location/consent",
            get(get_location_consent)
                .post(grant_location_consent)
                .delete(revoke_location_consent),
        )
        .with_state(state.consent.clone());

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
        .with_state(PhoneApiState {
            pool: state.pool.clone(),
            memory: state.memory.clone(),
        });

    let live_routes = Router::new()
        .route("/v1/me/events/socket", get(live_socket))
        .with_state(LiveApiState {
            hub: state.user_events.clone(),
            pool: state.pool.clone(),
        });

    let openapi_route = Router::new().route("/openapi.json", get(get_openapi_spec));

    let internal_routes = Router::new()
        .route(
            "/internal/v1/devices/dispatch",
            post(dispatch_device_request),
        )
        .with_state(state.clone());

    let protected_routes = span_routes
        .merge(collection_routes)
        .merge(record_routes)
        .merge(schema_routes)
        .merge(sms_routes)
        .merge(sms_consent_routes)
        .merge(location_routes)
        .merge(location_consent_routes)
        .merge(device_routes)
        .merge(device_socket_routes)
        .merge(event_routes)
        .merge(identity_routes)
        .merge(phone_routes)
        .merge(live_routes)
        .layer(middleware::from_fn_with_state(
            state.pool.clone(),
            extract_actor,
        ));

    let base_legacy_router = vox_core::http::router(state.legacy);

    base_legacy_router
        .merge(openapi_route)
        .merge(auth_routes)
        .merge(internal_routes)
        .merge(protected_routes)
}
