/**
* API service entry point running HTTP server and lifecycle listeners.
*/
mod auth;
mod config;
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
            .expect("Vox Core agent configuration is invalid")
            .with_device_hub(device_hub.clone())
            .with_user_events(user_events.clone()),
    );
    let cache = RedisContextCache::new(&config.redis_url)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ContextCache>);
    let memory = MemoryService::new(db.clone(), cache);
    let _ = memory.sync_minimal_users().await;
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    let jev_client = config
        .jev_api_key
        .as_ref()
        .map(|k| vox_core::jev::JevClient::new(k.clone(), Some(config.jev_base_url.clone())));
    let connected_apps = Arc::new(vox_core::connected_apps::ConnectedAppsService::from_config(
        db.clone(),
        &config,
    ));
    let mut legacy_state = AppState::with_memory_and_jev(
        db.clone(),
        agent,
        memory.clone(),
        config.service_token,
        jev_client,
    );
    legacy_state = legacy_state.with_connected_apps(connected_apps);
    if let Some(admin) = vox_core::http::admin::RedisAdmin::from_token_with_url(
        Some(config.redis_url.as_str()),
        std::env::var("VOX_ADMIN_TOKEN").ok(),
    )
    .expect("Vox admin Redis URL is invalid")
    {
        legacy_state = legacy_state.with_admin(admin);
    }
    if let Some(key) = config.status_webhook_key.as_deref() {
        let secrets = vox_core::status::EncryptedWebhookSecretStore::from_hex_key(db.clone(), key)
            .expect("VOX_STATUS_WEBHOOK_KEY must be a 32-byte hex key");
        legacy_state = legacy_state.with_status_secret_store(Arc::new(secrets));
    }
    let api_state = ApiState::new(
        legacy_state,
        db,
        device_hub,
        memory,
        user_events,
        config.google_maps_api_key.clone(),
    );
    let app = build_api_router(api_state);

    tracing::info!("Vox Core API listening on {}", config.bind_address);
    axum::serve(listener, app)
        .await
        .expect("Vox Core API server failed");
}
