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
    tracing_subscriber::fmt::init();
    let config = Config::from_env().expect("Vox Core configuration is invalid");

    let db = Db::connect(&config.database_url)
        .await
        .expect("Vox Core database is unavailable");
    db.migrate()
        .await
        .expect("Vox Core database migration failed");

    let agent = Arc::new(
        ConversationAgent::with_db(&config, db.clone())
            .expect("Vox Core agent configuration is invalid"),
    );
    let redis_url = config.redis_url.as_deref().unwrap_or("redis://redis:6379");
    let cache = RedisContextCache::new(redis_url)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ContextCache>);
    let memory = MemoryService::new(db.clone(), cache);
    let _ = memory.sync_greeting_names().await;
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    let jev_client = config
        .jev_api_key
        .as_ref()
        .map(|k| vox_core::jev::JevClient::new(k.clone(), Some(config.jev_base_url.clone())));
    let mut legacy_state =
        AppState::with_memory_and_jev(db.clone(), agent, memory, config.service_token, jev_client);
    if let Some(admin) = vox_core::http::admin::RedisAdmin::from_token_with_url(
        config.redis_url.as_deref().or(Some("redis://redis:6379")),
        std::env::var("VOX_ADMIN_TOKEN").ok(),
    )
    .expect("Vox admin Redis URL is invalid")
    {
        legacy_state = legacy_state.with_admin(admin);
    }
    if let Some(mut trust) = legacy_state.take_host_trust() {
        let redis_url = config
            .redis_url
            .clone()
            .unwrap_or_else(|| "redis://redis:6379".to_string());
        match redis::Client::open(redis_url.as_str()) {
            Ok(client) => match client.get_connection_manager().await {
                Ok(connection) => trust = trust.with_redis(connection),
                Err(error) => {
                    tracing::error!(%error, "host assertion replay store is unavailable");
                    trust = trust.require_shared_replay();
                }
            },
            Err(error) => {
                tracing::error!(%error, "host assertion replay store is unavailable");
                trust = trust.require_shared_replay();
            }
        }
        trust
            .load_durable_credentials(std::env::var("VOX_HOST_CREDENTIALS_SECRET").ok())
            .await
            .expect("host credentials are unavailable");
        legacy_state.set_host_trust(trust);
    }

    let api_state = ApiState::new(legacy_state, db);
    let app = build_api_router(api_state);

    tracing::info!("Vox Core API listening on {}", config.bind_address);
    axum::serve(listener, app)
        .await
        .expect("Vox Core API server failed");
}
