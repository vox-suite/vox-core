use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    agents::event_planner::GeminiEventPlanner,
    config::Config,
    db::{Db, jobs::JobRepository},
    events::handler::EventHandler,
    workers::Worker,
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
    let planner = Arc::new(
        GeminiEventPlanner::new(&config).expect("Vox Core planner configuration is invalid"),
    );
    let events = EventHandler::new(db.clone(), planner);
    let worker = Worker::new(JobRepository::new(db), events, Uuid::new_v4().to_string());
    let cancellation = CancellationToken::new();
    let shutdown = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Vox Core Worker shutdown listener failed");
        shutdown.cancel();
    });
    worker
        .run(cancellation)
        .await
        .expect("Vox Core Worker failed");
}
