use std::sync::Arc;
use vox_core::{
    agents::conversation::ConversationAgent,
    config::Config,
    db::Db,
    http::{AppState, router},
    memory::{
        MemoryService,
        cache::{ContextCache, RedisContextCache},
    },
};

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
    let cache = RedisContextCache::new("redis://redis:6379")
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
    let mut state =
        AppState::with_memory_and_jev(db, agent, memory, config.service_token, jev_client);
    if let Some(admin) =
        vox_core::http::admin::RedisAdmin::from_token(std::env::var("VOX_ADMIN_TOKEN").ok())
            .expect("Vox admin Redis URL is invalid")
    {
        state = state.with_admin(admin);
    }
    if let Some(token) = config.audit_admin_token {
        state = state.with_audit_admin_token(token);
    }

    axum::serve(listener, router(state))
        .await
        .expect("Vox Core API failed");
}
