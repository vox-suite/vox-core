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
        charts::{
            ChartApiState, create_chart_board, get_chart_board, get_chart_board_data,
            list_chart_boards, suggest_charts,
        },
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
        records::{create_record, delete_record, get_record, list_records, update_record},
        schemas::{create_schema_version, get_schema_by_name, list_schemas},
        sms::{get_consent, grant_consent, revoke_consent, submit_batch},
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

    let record_routes = Router::new()
        .route("/v1/records", post(create_record))
        .route("/v1/records/list", post(list_records))
        .route("/v1/records/{id}", post(get_record))
        .route("/v1/records/{id}/update", post(update_record))
        .route("/v1/records/{id}/delete", post(delete_record))
        .with_state(state.records.clone());

    let schema_routes = Router::new()
        .route("/v1/schemas", post(create_schema_version))
        .route("/v1/schemas/{namespace}/{name}", post(get_schema_by_name))
        .route("/v1/me/schemas", get(list_schemas))
        .with_state(state.schemas.clone());

    let sms_routes = Router::new()
        .route("/v1/sms/batches", post(submit_batch))
        .with_state(state.sms_ingestion.clone());

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

    let chart_api_state = ChartApiState {
        pool: state.pool.clone(),
        charts: state.charts.clone(),
        schemas: state.schemas.clone(),
        suggester: state.chart_suggester.clone(),
    };
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
            "/v1/me/pulse/suggestions",
            post(crate::routes::pulse::discover),
        )
        .route("/v1/me/pulse/preview", post(crate::routes::pulse::preview))
        .route("/v1/me/pulse/compose", post(crate::routes::pulse::compose))
        .route(
            "/v1/me/pulse/goals",
            get(crate::routes::pulse::list_goals).post(crate::routes::pulse::create_goal),
        )
        .route(
            "/v1/me/pulse/goals/compose",
            post(crate::routes::pulse::compose_goal),
        )
        .route(
            "/v1/me/pulse/goals/{id}",
            axum::routing::delete(crate::routes::pulse::delete_goal),
        )
        .route(
            "/v1/me/pulse/goals/{id}/entries",
            post(crate::routes::pulse::add_goal_entry),
        )
        .route("/v1/me/pulse/charts", post(crate::routes::pulse::save))
        .route(
            "/v1/me/pulse/charts/{id}",
            axum::routing::delete(crate::routes::pulse::delete_chart),
        )
        .route(
            "/v1/me/pulse/dismissals",
            post(crate::routes::pulse::dismiss),
        )
        .with_state(pulse_service.clone());
    let chart_routes = Router::new()
        .route("/v1/me/charts/suggest", post(suggest_charts))
        .route(
            "/v1/me/charts/boards",
            get(list_chart_boards).post(create_chart_board),
        )
        .route("/v1/me/charts/boards/{id}", get(get_chart_board))
        .route("/v1/me/charts/boards/{id}/data", get(get_chart_board_data))
        .with_state(chart_api_state);

    let space_api_state = crate::routes::spaces::SpaceApiState {
        pool: state.pool.clone(),
        spaces: state.spaces.clone(),
        schemas: state.schemas.clone(),
        architect: state.space_architect.clone(),
        runtime: state.space_runtime.clone(),
        user_events: state.user_events.clone(),
        pulse: pulse_service.clone(),
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
        .route(
            "/v1/me/spaces/{id}/nodes/{node_id}/goal",
            post(crate::routes::spaces::approve_node_goal),
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
        .route(
            "/v1/me/connections/{id}/reassociate",
            post(crate::routes::connections::reassociate_connection),
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

    let protected_routes = span_routes
        .merge(collection_routes)
        .merge(record_routes)
        .merge(schema_routes)
        .merge(sms_routes)
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
        .merge(chart_routes)
        .merge(pulse_routes)
        .merge(space_routes)
        .merge(web_token_routes)
        .merge(connection_routes)
        .layer(middleware::from_fn_with_state(
            state.pool.clone(),
            extract_actor,
        ));

    let base_legacy_router = vox_core::http::router(state.legacy);

    base_legacy_router
        .merge(openapi_route)
        .merge(auth_routes)
        .merge(internal_routes)
        .merge(google_callback_route)
        .merge(protected_routes)
        .layer(crate::cors::layer())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use vox_core::{config::Config, db::Db, http::AppState, memory::MemoryService};

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL"]
    async fn production_api_router_assembles_without_overlapping_routes() {
        let database_url = std::env::var("TEST_DATABASE_URL").unwrap();
        let config = Config::from_values(|name| match name {
            "DATABASE_URL" => Some(database_url.clone()),
            "VOX_AUTH_TOKEN" | "GEMINI_API_KEY" | "EXA_API_KEY" => Some("test-only".into()),
            _ => None,
        })
        .unwrap();
        let db = Db::connect(&database_url).await.unwrap();
        let memory = MemoryService::new(db.clone(), None);
        let state = ApiState::new(
            AppState::new(true),
            db.clone(),
            vox_core::realtime::DeviceHub::new(),
            memory,
            vox_core::realtime::UserEventHub::new(),
            &config,
            None,
            None,
            Arc::new(vox_core::agents::chart_suggester::GeminiChartSuggester::new(&config)),
            Arc::new(vox_core::agents::space_architect::GeminiSpaceArchitect::new(&config)),
            Arc::new(vox_core::agents::space_runtime::SpaceRuntime::new(
                db, &config, None,
            )),
        );
        // Exercises the exact assembly used by the API binary, including both
        // public OAuth callbacks and authenticated account operations.
        let router = build_api_router(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let client = reqwest::Client::new();
        let live = client
            .get(format!("http://{address}/health/live"))
            .send()
            .await
            .unwrap();
        assert_eq!(live.status(), reqwest::StatusCode::OK);
        let reassociate = client
            .post(format!(
                "http://{address}/v1/me/connections/{}/reassociate",
                uuid::Uuid::nil()
            ))
            .json(&serde_json::json!({}))
            .send()
            .await
            .unwrap();
        server.abort();
        assert_eq!(reassociate.status(), reqwest::StatusCode::UNAUTHORIZED);
    }
}
