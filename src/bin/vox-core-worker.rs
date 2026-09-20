use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    actions::handler::ActionHandler,
    agents::{event_planner::GeminiEventPlanner, summarizer::GeminiSummarizer},
    bridge_client::BridgeClient,
    config::Config,
    db::{Db, jobs::JobRepository},
    events::handler::EventHandler,
    memory::{
        MemoryService,
        cache::{ContextCache, RedisContextCache},
    },
    schedules::{handler::ScheduleHandler, ticker::ScheduleTicker},
    summaries::handler::SummaryHandler,
    workers::{Worker, task_executor::TaskExecutorHandler, whatsapp_sweeper::WhatsAppSweeper},
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
    let cache = config.redis_url.as_deref().map(|url| {
        Arc::new(RedisContextCache::new(url).expect("Vox Core Redis URL is invalid"))
            as Arc<dyn ContextCache>
    });
    let memory = MemoryService::new(db.clone(), cache);

    let (triager, schema_classifier, jev_client) = if let Some(ref api_key) = config.jev_api_key {
        let client = vox_core::jev::client::JevClient::new(
            api_key.clone(),
            Some(config.jev_base_url.clone()),
        );
        (
            Some(Arc::new(vox_core::jev::event_triage::EventTriager::new(
                client.clone(),
            ))),
            Some(Arc::new(
                vox_core::jev::schema_classifier::SchemaClassifier::new(client.clone(), db.clone()),
            )),
            Some(client),
        )
    } else {
        (None, None, None)
    };
    let events = EventHandler::with_jev(
        db.clone(),
        planner.clone(),
        memory.clone(),
        triager,
        schema_classifier,
    );
    let schedules =
        ScheduleHandler::with_jev(db.clone(), planner, memory.clone(), jev_client.clone());
    let ticker = ScheduleTicker::new(db.clone());

    let bridge_url = config
        .bridge_url
        .clone()
        .unwrap_or_else(|| "http://bridge:3000".to_string());
    let bridge_client = Arc::new(
        BridgeClient::new(bridge_url, config.service_token.clone())
            .expect("Vox Core bridge client creation failed"),
    );
    let actions = ActionHandler::new(db.clone(), bridge_client);
    let summarizer = Arc::new(GeminiSummarizer::new(&config));
    let summaries =
        SummaryHandler::with_jev(db.clone(), summarizer, memory.clone(), jev_client.clone());
    let task_executor = TaskExecutorHandler::with_jev(db.clone(), &config, jev_client);
    let wa_sweeper = WhatsAppSweeper::new(db.clone());

    let worker = Worker::with_all_handlers(
        JobRepository::new(db),
        events,
        schedules,
        ticker,
        actions,
        summaries,
        task_executor,
        wa_sweeper,
        Uuid::new_v4().to_string(),
    );
    let cancellation = CancellationToken::new();
    let greeting_sync = tokio::spawn(memory.run_greeting_sync(cancellation.clone()));
    let shutdown = cancellation.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Vox Core Worker shutdown listener failed");
        shutdown.cancel();
    });
    worker
        .run(cancellation.clone())
        .await
        .expect("Vox Core Worker failed");
    cancellation.cancel();
    let _ = greeting_sync.await;
}
