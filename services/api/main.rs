/**
* API service entry point running HTTP server and lifecycle listeners.
*/
mod auth;
mod config;
mod cors;
mod identity_token;
mod openapi;
mod router;
mod routes;
mod state;

use std::sync::Arc;
use vox_core::{
    agents::conversation::ConversationAgent,
    config::Config,
    db::Db,
    http::AppState,
    memory::{
        MemoryService,
        cache::{ContextCache, RedisContextCache},
    },
};

use crate::{router::build_api_router, state::ApiState};

#[tokio::main]
async fn main() {
    if std::env::args().any(|arg| arg == "--print-openapi") {
        println!(
            "{}",
            serde_json::to_string_pretty(&openapi::build_spec()).unwrap_or_default()
        );
        return;
    }
    let _traces = vox_core::telemetry::init("vox-core-api");
    let config = Config::from_env().expect("Vox Core configuration is invalid");

    let db = Db::connect_with_pool(
        &config.database_url,
        config.db_max_connections,
        config.db_acquire_timeout_secs,
    )
    .await
    .expect("Vox Core database is unavailable");
    db.migrate()
        .await
        .expect("Vox Core database migration failed");

    let device_hub = vox_core::realtime::DeviceHub::new();
    let user_events = vox_core::realtime::UserEventHub::new();
    let agent = Arc::new(
        ConversationAgent::with_db(&config, db.clone())
            .expect("Vox Core agent configuration is invalid"),
    );
    let cache = RedisContextCache::new(&config.redis_url)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ContextCache>);
    let memory = MemoryService::new(db.clone(), cache);
    let _ = memory.sync_minimal_users().await;
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    let connected_apps = Arc::new(vox_core::connected_apps::from_config(db.clone(), &config));
    let mut app_state = AppState::with_memory(
        db.clone(),
        agent,
        memory.clone(),
        config.service_token.clone(),
    );
    app_state = app_state.with_connected_apps(connected_apps);
    if let Ok(token) = std::env::var("VOX_ADMIN_TOKEN") {
        app_state = app_state.with_admin_token(token);
    }
    if let Some(key) = config.status_webhook_key.as_deref() {
        let secrets = vox_core::status::EncryptedWebhookSecretStore::from_hex_key(db.clone(), key)
            .expect("VOX_STATUS_WEBHOOK_KEY must be a 32-byte hex key");
        app_state = app_state.with_status_secret_store(Arc::new(secrets));
    }
    let tts = config.elevenlabs_api_key.as_ref().map(|key| {
        Arc::new(vox_core::tts::ElevenLabsClient::new(
            key.clone(),
            config.elevenlabs_model_id.clone(),
            config.elevenlabs_voice_id.clone(),
            config.elevenlabs_output_format.clone(),
        ))
    });
    let stt = config.assemblyai_api_key.as_ref().map(|key| {
        Arc::new(vox_core::stt::AssemblyAiClient::new(
            key.clone(),
            config.assemblyai_speech_model.clone(),
        ))
    });
    let chart_suggester: Arc<dyn vox_core::agents::chart_suggester::SuggestingCharts> =
        Arc::new(vox_core::agents::chart_suggester::GeminiChartSuggester::new(&config));
    let space_architect: Arc<dyn vox_core::agents::space_architect::SpaceArchitecting> =
        Arc::new(vox_core::agents::space_architect::GeminiSpaceArchitect::new(&config));
    let space_runtime = Arc::new(vox_core::agents::space_runtime::SpaceRuntime::new(
        db.clone(),
        &config,
        Some(user_events.clone()),
    ));
    let api_state = ApiState::new(
        app_state,
        db,
        device_hub,
        memory,
        user_events,
        config.google_maps_api_key.clone(),
        tts,
        stt,
        chart_suggester,
        space_architect,
        space_runtime,
    );
    let app = build_api_router(api_state);

    tracing::info!("Vox Core API listening on {}", config.bind_address);
    axum::serve(listener, app)
        .await
        .expect("Vox Core API server failed");
}
