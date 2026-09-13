use std::sync::Arc;
use vox_core::{
    agents::conversation::ConversationAgent,
    config::Config,
    db::Db,
    http::{AppState, router},
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
    let agent =
        Arc::new(ConversationAgent::new(&config).expect("Vox Core agent configuration is invalid"));
    let listener = tokio::net::TcpListener::bind(&config.bind_address)
        .await
        .expect("Vox Core API address is unavailable");
    axum::serve(
        listener,
        router(AppState::with_dependencies(db, agent, config.service_token)),
    )
    .await
    .expect("Vox Core API failed");
}
