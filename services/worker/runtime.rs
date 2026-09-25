/**
* Task worker dispatch loops, job claiming, and execution runtime.
*/
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    agents::{
        event_planner::GeminiEventPlanner, sms_extractor::GeminiSmsExtractor,
        summarizer::GeminiSummarizer,
    },
    bridge_client::BridgeClient,
    config::Config,
    core_api_client::{CoreApiClient, DeviceDispatcher},
    db::{Db, jobs::JobRepository},
    events::handler::EventHandler,
    memory::{
        MemoryService,
        cache::{ContextCache, RedisContextCache},
    },
    outbound::OutboundCallService,
    schedules::{handler::ScheduleHandler, ticker::ScheduleTicker},
    sms_ingestion::{handler::SmsBatchHandler, retention::SmsRetentionSweeper},
    status::{EncryptedWebhookSecretStore, StatusService},
    summaries::handler::SummaryHandler,
    workers::{Worker, task_executor::TaskExecutorHandler, whatsapp_sweeper::WhatsAppSweeper},
};

pub async fn run_worker(
    config: Config,
    cancellation: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::connect(&config.database_url)
        .await
        .expect("Vox Core database is unavailable");
    let planner = Arc::new(
        GeminiEventPlanner::new(&config).expect("Vox Core planner configuration is invalid"),
    );
    let redis_url = config.redis_url.as_deref().unwrap_or("redis://redis:6379");
    let cache = RedisContextCache::new(redis_url)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ContextCache>);
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

    let bridge_client = config.bridge_url.as_ref().and_then(|url| {
        BridgeClient::new(url.clone(), config.service_token.clone())
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn vox_core::bridge_client::OutboundBridge>)
    });
    let outbound = Arc::new(OutboundCallService::new(db.clone(), bridge_client));

    let events = EventHandler::with_jev(
        db.clone(),
        planner.clone(),
        memory.clone(),
        triager,
        schema_classifier,
    );
    let mut schedules =
        ScheduleHandler::with_jev(db.clone(), planner, memory.clone(), jev_client.clone());
    schedules = schedules.with_outbound(outbound.clone());
    let ticker = ScheduleTicker::new(db.clone());

    let summarizer = Arc::new(GeminiSummarizer::new(&config));
    let summaries =
        SummaryHandler::with_jev(db.clone(), summarizer, memory.clone(), jev_client.clone());
    let mut task_executor = TaskExecutorHandler::with_jev(db.clone(), &config, jev_client);
    task_executor = task_executor.with_outbound(outbound);
    let wa_sweeper = WhatsAppSweeper::new(db.clone());
    let sms_extractor = Arc::new(GeminiSmsExtractor::new(&config));
    let device_dispatcher = config.core_api_url.as_ref().and_then(|url| {
        CoreApiClient::new(url.clone(), config.service_token.clone())
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn DeviceDispatcher>)
    });
    let sms_batches = SmsBatchHandler::new(db.clone(), sms_extractor, device_dispatcher);
    let sms_retention = SmsRetentionSweeper::new(db.clone());

    let worker_id = Uuid::new_v4().to_string();
    let status_handle = if let Some(key) = config.status_webhook_key.as_deref() {
        let secrets = EncryptedWebhookSecretStore::from_hex_key(db.clone(), key)?;
        let delivery = StatusService::new(db.clone())
            .with_secret_store(Arc::new(secrets))
            .delivery_worker();
        let status_cancel = cancellation.clone();
        let status_worker_id = worker_id.clone();
        Some(tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = status_cancel.cancelled() => break,
                    result = delivery.deliver_next(&status_worker_id, chrono::Utc::now()) => {
                        match result {
                            Ok(true) => continue,
                            Ok(false) => {},
                            Err(error) => tracing::warn!("Status webhook delivery unavailable: {}", error),
                        }
                    }
                }
                tokio::select! {
                    _ = status_cancel.cancelled() => break,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
                }
            }
        }))
    } else {
        None
    };
    let worker = Worker::with_all_handlers(
        JobRepository::new(db),
        events,
        schedules,
        ticker,
        summaries,
        task_executor,
        wa_sweeper,
        sms_batches,
        sms_retention,
        worker_id,
    );

    tokio::spawn(memory.run_greeting_sync(cancellation.clone()));

    let result = worker.run(cancellation.clone()).await;
    cancellation.cancel();
    if let Some(handle) = status_handle {
        handle.await?;
    }
    result?;
    Ok(())
}
