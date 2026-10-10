/**
* Route definitions and Axum state assembly for the public HTTP API.
*/
use axum::{
    Router, middleware,
    routing::{get, patch, post},
};

use crate::{
    auth::extract_actor,
    openapi::get_openapi_spec,
    routes::{
        auth::exchange_token,
        client_logs::submit_client_logs,
        collections::{
            add_collection_span, archive_collection, create_collection, get_collection,
            list_collections, remove_collection_span, update_collection,
        },
        device_socket::{DeviceSocketState, device_socket},
        devices::{DeviceApiState, heartbeat, register_device},
        events::ingest_batch,
        identity::get_me,
        internal::dispatch_device_request,
        live::{LiveApiState, live_socket},
        map_scene::get_map_scene,
        phone::{PhoneApiState, confirm_phone_verification, link_phone, start_phone_verification},
        schemas::{create_schema_version, get_schema_by_name, list_schemas},
        sms::{get_consent, grant_consent, revoke_consent},
        spans::{
            create_span, delete_span, get_span, list_span_day, list_span_days, list_spans,
            update_span,
        },
        tools::{ToolsApiState, invoke_tool, list_tools},
        voice::{VoiceSocketState, voice_socket},
    },
    state::ApiState,
};

pub fn build_api_router(state: ApiState) -> Router {
    let integration_service = vox_core::integrations::IntegrationService::new(
        state.pool.clone(),
        state.spans.clone(),
        state.collections.clone(),
        state.charts.clone(),
    );
    let integration_public =
        vox_core::integrations::http::public_router(integration_service.clone());
    let integration_authorize =
        vox_core::integrations::http::authorization_router(integration_service);
    let auth_routes = Router::new()
        .route("/v1/auth/exchange", post(exchange_token))
        .with_state(state.pool.clone());

    let span_routes = Router::new()
        .route("/v1/spans", post(create_span))
        .route("/v1/spans/list", post(list_spans))
        .route("/v1/spans/days", post(list_span_days))
        .route("/v1/spans/day", post(list_span_day))
        .route("/v1/spans/{id}", post(get_span))
        .route("/v1/spans/{id}/update", post(update_span))
        .route("/v1/spans/{id}/delete", post(delete_span))
        .with_state(state.spans.clone());

    let collection_routes = Router::new()
        .route("/v1/collections", post(create_collection))
        .route("/v1/collections/list", post(list_collections))
        .route("/v1/collections/{id}", post(get_collection))
        .route("/v1/collections/{id}/update", post(update_collection))
        .route("/v1/collections/{id}/archive", post(archive_collection))
        .route(
            "/v1/collections/{id}/spans/{span_id}/add",
            post(add_collection_span),
        )
        .route(
            "/v1/collections/{id}/spans/{span_id}/remove",
            post(remove_collection_span),
        )
        .with_state(state.collections.clone());

    let schema_routes = Router::new()
        .route("/v1/schemas", post(create_schema_version))
        .route("/v1/schemas/{namespace}/{name}", post(get_schema_by_name))
        .route("/v1/me/schemas", get(list_schemas))
        .with_state(state.schemas.clone());

    let refresh_cache = middleware::from_fn_with_state(
        state.memory.clone(),
        crate::cache_refresh::refresh_minimal_user_after,
    );

    let sms_consent_routes = Router::new()
        .route("/v1/sms/consent/get", post(get_consent))
        .route(
            "/v1/sms/consent/grant",
            post(grant_consent).layer(refresh_cache.clone()),
        )
        .route(
            "/v1/sms/consent/revoke",
            post(revoke_consent).layer(refresh_cache.clone()),
        )
        .with_state(state.consent.clone());

    let client_log_routes = Router::new().route("/v1/logs/batches", post(submit_client_logs));

    let device_api_state = DeviceApiState {
        devices: state.devices.clone(),
    };
    let device_routes = Router::new()
        .route(
            "/v1/devices",
            post(register_device).layer(refresh_cache.clone()),
        )
        .route("/v1/devices/{id}/heartbeat", post(heartbeat))
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
        .route("/v1/me", post(get_me))
        .with_state(state.pool.clone());

    let phone_routes = Router::new()
        .route("/v1/me/phone", post(link_phone))
        .route("/v1/me/phone/verify/start", post(start_phone_verification))
        .route(
            "/v1/me/phone/verify/confirm",
            post(confirm_phone_verification),
        )
        .with_state(PhoneApiState {
            pool: state.pool.clone(),
            memory: state.memory.clone(),
            verification: vox_core::phone_verification::PhoneVerificationService::new(
                state.pool.clone(),
            ),
            bridge: state.bridge.clone(),
        });

    let live_routes = Router::new()
        .route("/v1/me/events/socket", get(live_socket))
        .with_state(LiveApiState {
            hub: state.user_events.clone(),
            pool: state.pool.clone(),
        });

    let map_scene_routes = Router::new()
        .route("/v1/me/map/scene", get(get_map_scene))
        .with_state(state.user_events.clone());

    let voice_routes = Router::new()
        .route("/v1/me/voice/socket", get(voice_socket))
        .with_state(VoiceSocketState {
            conversations: state.legacy.conversations(),
            tts: state.tts.clone(),
            stt: state.stt.clone(),
        });

    let tool_routes = Router::new()
        .route("/v1/me/tools", get(list_tools))
        .route("/v1/me/tools/{name}", post(invoke_tool))
        .with_state(ToolsApiState {
            conversations: state.legacy.conversations(),
            export: state.tool_export.clone(),
        });

    let pulse_service = vox_core::application::pulse::service::PulseService::new(
        vox_core::storage::pulse::PulseRepository::new(state.pool.clone()),
    )
    .with_suggester(state.chart_suggester.clone());
    let pulse_routes = Router::new()
        .route("/v1/me/pulse/canvas", get(crate::routes::pulse::get_canvas))
        .route(
            "/v1/me/pulse/measurements",
            get(crate::routes::pulse::list_measurements),
        )
        .route(
            "/v1/me/pulse/discover",
            get(crate::routes::pulse::list_measurements),
        )
        .route(
            "/v1/me/pulse/suggestions",
            post(crate::routes::pulse::discover),
        )
        .route("/v1/me/pulse/preview", post(crate::routes::pulse::preview))
        .route(
            "/v1/me/pulse/charts",
            get(crate::routes::pulse::list_charts).post(crate::routes::pulse::save),
        )
        .route(
            "/v1/me/pulse/charts/{id}",
            get(crate::routes::pulse::get_chart)
                .patch(crate::routes::pulse::update_chart)
                .delete(crate::routes::pulse::delete_chart),
        )
        .route(
            "/v1/me/pulse/dismissals",
            get(crate::routes::pulse::list_dismissals).post(crate::routes::pulse::dismiss),
        )
        .route(
            "/v1/me/pulse/dismissals/{key}",
            axum::routing::delete(crate::routes::pulse::undismiss),
        )
        .with_state(pulse_service);

    let space_api_state = crate::routes::spaces::SpaceApiState {
        pool: state.pool.clone(),
        spaces: state.spaces.clone(),
        runtime: state.space_runtime.clone(),
        user_events: state.user_events.clone(),
    };
    let space_routes = Router::new()
        .route(
            "/v1/me/spaces",
            get(crate::routes::spaces::list_spaces).post(crate::routes::spaces::create_space),
        )
        .route(
            "/v1/me/spaces/{id}",
            get(crate::routes::spaces::get_space).delete(crate::routes::spaces::drop_space),
        )
        .route(
            "/v1/me/spaces/{id}/chat",
            post(crate::routes::spaces::send_space_chat),
        )
        .route(
            "/v1/me/spaces/{id}/stop",
            post(crate::routes::spaces::stop_space),
        )
        .route(
            "/v1/me/spaces/{id}/nodes/{node}/retry",
            post(crate::routes::spaces::retry_space_node),
        )
        .route(
            "/v1/me/spaces/{id}/commit",
            post(crate::routes::spaces::commit_space),
        )
        .route(
            "/v1/me/spaces/{id}/messages",
            get(crate::routes::spaces::list_space_messages),
        )
        .route(
            "/v1/me/spaces/{id}/nodes/{node_id}",
            patch(crate::routes::spaces::update_space_node),
        )
        .with_state(space_api_state);

    let web_token_routes = Router::new()
        .route(
            "/v1/auth/web-token",
            post(crate::routes::auth::mint_web_token),
        )
        .with_state(state.pool.clone());

    let openapi_route = Router::new().route("/openapi.json", get(get_openapi_spec));

    let internal_routes = Router::new()
        .route(
            "/internal/v1/devices/dispatch",
            post(dispatch_device_request),
        )
        .with_state(state.clone());

    let connection_routes = Router::new()
        .route(
            "/v1/me/connections/{id}/read",
            post(crate::routes::connections::read_personal),
        )
        .route(
            "/v1/me/connections/youtube/history/import",
            post(crate::routes::connections::import_youtube_history)
                .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/v1/me/connections/maps_timeline/history/import",
            post(crate::routes::connections::import_maps_timeline)
                .layer(axum::extract::DefaultBodyLimit::max(16 * 1024 * 1024)),
        )
        .route(
            "/v1/me/connections/setup/{id}/cancel",
            post(crate::routes::connections::cancel_setup),
        )
        .route(
            "/v1/me/connectors/list",
            post(crate::routes::connections::list_connectors),
        )
        .route(
            "/v1/me/connections/list",
            post(crate::routes::connections::list_connections),
        )
        .route(
            "/v1/me/connections/start",
            post(crate::routes::connections::start_connection),
        )
        .route(
            "/v1/me/connections/setup/{id}/status",
            post(crate::routes::connections::setup_status),
        )
        .route(
            "/v1/me/connections/{id}/preferences",
            post(crate::routes::connections::update_preferences),
        )
        .route(
            "/v1/me/connections/{id}/refresh",
            post(crate::routes::connections::refresh_connection),
        )
        .route(
            "/v1/me/connections/{id}/disconnect",
            post(crate::routes::connections::disconnect_connection),
        )
        .with_state(state.connections.clone());

    let google_callback_route = Router::new()
        .route(
            "/v1/connectors/google/callback",
            get(crate::routes::connections::google_callback),
        )
        .route(
            "/v1/connectors/{connector}/callback",
            get(crate::routes::connections::connector_callback),
        )
        .with_state(state.connections.clone());

    let timeline_routes = Router::new()
        .route(
            "/v1/timeline/events/counts",
            post(crate::routes::timeline::day_counts),
        )
        .route(
            "/v1/timeline/groups",
            get(crate::routes::timeline::list_groups),
        )
        .route(
            "/v1/timeline/event-types",
            get(crate::routes::timeline::list_event_types)
                .post(crate::routes::timeline::create_event_type),
        )
        .route(
            "/v1/timeline/events/query",
            post(crate::routes::timeline::query_events),
        )
        .route(
            "/v1/timeline/events",
            post(crate::routes::timeline::ingest_event),
        )
        .with_state(state.timeline.clone());

    let update_routes = Router::new()
        .route(
            "/v1/updates/list",
            post(crate::routes::updates::list_updates),
        )
        .route("/v1/updates/{id}", get(crate::routes::updates::get_update))
        .route(
            "/v1/updates/{id}/read",
            post(crate::routes::updates::mark_update_read),
        )
        .route(
            "/v1/updates/{id}/dismiss",
            post(crate::routes::updates::dismiss_update),
        )
        .route(
            "/v1/updates/{id}/resolve",
            post(crate::routes::updates::resolve_update),
        )
        .route(
            "/v1/updates/jobs/{id}/retry",
            post(crate::routes::updates::retry_job),
        )
        .route(
            "/v1/updates/jobs/{id}/input",
            post(crate::routes::updates::provide_job_input),
        )
        .with_state(state.updates.clone());

    let connector_ingest_routes = Router::new()
        .route(
            "/v1/connectors/gmail/device-access",
            post(crate::routes::gmail::device_access),
        )
        .route(
            "/v1/connectors/gmail/device-historical-import",
            post(crate::routes::gmail::device_historical_import)
                .layer(axum::extract::DefaultBodyLimit::max(40 * 1024 * 1024)),
        )
        .route(
            "/v1/connectors/google/takeout/upload",
            post(crate::routes::takeout::upload_takeout)
                .layer(axum::extract::DefaultBodyLimit::max(104_857_600))
                .layer(middleware::from_fn(crate::routes::takeout::limit_import)),
        )
        .with_state(state.pool.clone());

    let gmail_pubsub_route = Router::new()
        .route(
            "/v1/connectors/gmail/pubsub",
            post(crate::routes::gmail::pubsub_webhook),
        )
        .with_state(state.pool.clone());

    let protected_routes = span_routes
        .merge(integration_authorize)
        .merge(collection_routes)
        .merge(schema_routes)
        .merge(client_log_routes)
        .merge(sms_consent_routes)
        .merge(device_routes)
        .merge(device_socket_routes)
        .merge(event_routes)
        .merge(identity_routes)
        .merge(phone_routes)
        .merge(live_routes)
        .merge(map_scene_routes)
        .merge(voice_routes)
        .merge(tool_routes)
        .merge(pulse_routes)
        .merge(space_routes)
        .merge(web_token_routes)
        .merge(connection_routes)
        .merge(timeline_routes)
        .merge(update_routes)
        .merge(connector_ingest_routes)
        .layer(middleware::from_fn_with_state(
            state.pool.clone(),
            extract_actor,
        ));

    let base_legacy_router = vox_core::http::router(state.legacy);

    base_legacy_router
        .merge(openapi_route)
        .merge(auth_routes)
        .merge(integration_public)
        .merge(internal_routes)
        .merge(google_callback_route)
        .merge(gmail_pubsub_route)
        .merge(protected_routes)
        .layer(crate::cors::layer())
}
