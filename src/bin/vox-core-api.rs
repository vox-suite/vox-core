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
    let cache = config.redis_url.as_deref().map(|url| {
        Arc::new(RedisContextCache::new(url).expect("Vox Core Redis URL is invalid"))
            as Arc<dyn ContextCache>
    });
    let memory = MemoryService::new(db.clone(), cache);
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    let mut state = AppState::with_memory(db, agent, memory, config.service_token);
    if let Ok(token) = std::env::var("VOX_ADMIN_TOKEN")
        && !token.trim().is_empty()
    {
        state = state.with_admin(
            vox_core::http::admin::RedisAdmin::new(config.redis_url.as_deref(), token)
                .expect("Vox admin Redis URL is invalid"),
        );
    }
    axum::serve(listener, router(state))
        .await
        .expect("Vox Core API failed");
}
