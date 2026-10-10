use sqlx::Row;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use vox_core::{
    agents::{
        event_planner::GeminiEventPlanner, schema_extractor::GeminiSchemaExtractor,
        summarizer::GeminiSummarizer,
    },
    bridge_client::BridgeClient,
    config::Config,
    db::{Db, jobs::JobRepository},
    events::handler::EventHandler,
    memory::{
        MemoryService,
        cache::{ContextCache, RedisContextCache},
    },
    outbound::OutboundCallService,
    schedules::{handler::ScheduleHandler, ticker::ScheduleTicker},
    status::{EncryptedWebhookSecretStore, StatusService},
    summaries::handler::SummaryHandler,
    workers::{Worker, task_executor::TaskExecutorHandler, whatsapp_sweeper::WhatsAppSweeper},
};

pub async fn run_worker(
    config: Config,
    cancellation: CancellationToken,
) -> Result<(), Box<dyn std::error::Error>> {
    let db = Db::connect_with_pool(
        &config.database_url,
        config.db_max_connections,
        config.db_acquire_timeout_secs,
    )
    .await
    .expect("Vox Core database is unavailable");
    let fresh_connections = vox_core::fresh_connections::FreshConnectionsService::new(
        db.pool().clone(),
        config.credential_key.as_deref(),
        None,
        config.google_client_id.clone(),
        config.google_client_secret.clone(),
        config.core_api_url.clone(),
    )?;
    let connections_cancel = cancellation.clone();
    let connections_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = connections_cancel.cancelled() => break,
                _ = interval.tick() => {
                    tokio::select! {
                        _ = connections_cancel.cancelled() => break,
                        result = fresh_connections.run_due_syncs() => {
                            if result.is_err() { tracing::warn!("Connections background sync unavailable"); }
                        }
                    }
                }
            }
        }
    });
    let gmail_pool = db.pool().clone();
    let gmail_cancel = cancellation.clone();
    let gmail_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(900));
        loop {
            tokio::select! {
                _ = gmail_cancel.cancelled() => break,
                _ = interval.tick() => {
                    tokio::select! {
                        _ = gmail_cancel.cancelled() => break,
                        _ = vox_core::gmail_sync::reconcile_all_gmail_connections(&gmail_pool) => {}
                    }
                }
            }
        }
    });
    let pulse_pool = db.pool().clone();
    let pulse_cancel = cancellation.clone();
    let pulse_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = pulse_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let _ = vox_core::storage::pulse::PulseRepository::process_pending_pulse_invalidations(&pulse_pool).await;
                }
            }
        }
    });
    let retention_sweeper =
        vox_core::attachments::AttachmentRetentionSweeper::new(db.pool().clone());
    let retention_cancel = cancellation.clone();
    let retention_handle = tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
        loop {
            tokio::select! {
                _ = retention_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let _ = retention_sweeper.sweep_expired().await;
                }
            }
        }
    });
    let key = vox_core::attachments::attachment_master_key()?;
    let att_pool = db.pool().clone();
    let att_cancel = cancellation.clone();
    let att_handle = tokio::spawn(async move {
        let att_worker_id = Uuid::new_v4().to_string();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = att_cancel.cancelled() => break,
                _ = interval.tick() => {
                    let claim_sql = "UPDATE jobs SET \
                        state = 'running', \
                        lease_owner = $1, \
                        lease_expires_at = now() + interval '5 minutes', \
                        lease_generation = lease_generation + 1 \
                     WHERE id = ( \
                         SELECT j.id FROM jobs j \
                         JOIN source_attachments a ON a.id = j.payload_reference_id AND a.user_id = j.user_id \
                         WHERE a.parse_state IN ('pending','processing') AND a.raw_deleted_at IS NULL \
                           AND j.kind='process_attachment' AND j.wait_reason IS NULL AND j.available_at<=now() AND (j.state='pending' OR (j.state='running' AND j.lease_expires_at<now())) \
                           AND (j.lease_expires_at IS NULL OR j.lease_expires_at < now()) \
                         LIMIT 1 \
                         FOR UPDATE OF j SKIP LOCKED \
                     ) \
                     RETURNING id, user_id, payload_reference_id";

                    let claimed = sqlx::query(claim_sql)
                        .bind(&att_worker_id)
                        .fetch_optional(&att_pool)
                        .await;

                    if let Ok(Some(row)) = claimed {
                        let j_id: Uuid = row.get("id");
                        let u_id: Uuid = row.get("user_id");
                        let att_id: Option<Uuid> = row.get("payload_reference_id");
                        if let Some(a_id) = att_id {
                            let attempt: Result<Uuid,sqlx::Error> = sqlx::query_scalar("INSERT INTO job_attempts(job_id,attempt_number,lease_generation,executor_kind) SELECT id,COALESCE((SELECT max(attempt_number) FROM job_attempts WHERE job_id=$1),0)+1,lease_generation,'server' FROM jobs WHERE id=$1 AND lease_owner=$2 RETURNING id")
                                .bind(j_id).bind(&att_worker_id).fetch_one(&att_pool).await;
                            let Ok(attempt_id) = attempt else {
                                tracing::error!(job_id=%j_id,"cannot record attachment attempt");
                                continue;
                            };
                            let result = vox_core::attachments::process_attachment(&att_pool, &key, u_id, a_id, j_id).await;
                            let (outcome,error_code) = match &result {
                                Ok(vox_core::attachments::AttachmentOutcome::Success) => ("succeeded",None),
                                Ok(vox_core::attachments::AttachmentOutcome::WaitingUser(_)) => ("failed",Some("user_input_required")),
                                Ok(vox_core::attachments::AttachmentOutcome::Failed(_)) => ("failed",Some("attachment_processing_failed")),
                                Err(_) => ("failed",Some("attachment_worker_failure")),
                            };
                            if let Err(error) = sqlx::query("UPDATE job_attempts SET finished_at=now(),heartbeat_at=now(),outcome=$1,error_details=$2 WHERE id=$3")
                                .bind(outcome).bind(error_code).bind(attempt_id).execute(&att_pool).await {
                                tracing::error!(job_id=%j_id,%error,"cannot finalize attachment attempt");
                            }
                            if let Err(error) = result {
                                tracing::error!(job_id=%j_id, %error, "attachment worker failed");
                                let _ = sqlx::query("UPDATE jobs SET state=CASE WHEN attempt_count+1>=max_attempts THEN 'failed' ELSE 'pending' END,attempt_count=attempt_count+1,lease_owner=NULL,lease_expires_at=NULL,available_at=now()+interval '30 seconds',last_error_code='attachment_worker_failure' WHERE id=$1 AND lease_owner=$2")
                                    .bind(j_id).bind(&att_worker_id).execute(&att_pool).await;
                            }
                        }
                    }
                }
            }
        }
    });
    let planner = Arc::new(
        GeminiEventPlanner::new(&config).expect("Vox Core planner configuration is invalid"),
    );
    let cache = RedisContextCache::new(&config.redis_url)
        .ok()
        .map(|c| Arc::new(c) as Arc<dyn ContextCache>);
    let memory = MemoryService::new(db.clone(), cache);

    let (triager, schema_classifier, schema_extractor, jev_client) = if let Some(ref api_key) =
        config.jev_api_key
    {
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
            Some(Arc::new(GeminiSchemaExtractor::new(&config))
                as Arc<
                    dyn vox_core::agents::schema_extractor::SchemaExtracting,
                >),
            Some(client),
        )
    } else {
        (None, None, None, None)
    };

    let bridge_client = config.bridge_url.as_ref().and_then(|url| {
        BridgeClient::new(url.clone(), config.service_token.clone())
            .ok()
            .map(|c| Arc::new(c) as Arc<dyn vox_core::bridge_client::OutboundBridge>)
    });
    let outbound = Arc::new(OutboundCallService::new(db.clone(), bridge_client));

    let notifier = match (&config.smtp_url, &config.email_from) {
        (Some(url), Some(from)) => vox_core::user_notifications::SmtpEmailSender::new(url, from)
            .ok()
            .map(|sender| {
                Arc::new(vox_core::user_notifications::UserNotifier::new(
                    db.pool().clone(),
                    Arc::new(sender),
                ))
            }),
        _ => None,
    };
    let mut events =
        EventHandler::with_jev(db.clone(), triager, schema_classifier, schema_extractor);
    if config.event_agent_enabled {
        events = events.with_agent(Arc::new(vox_core::agents::event_agent::EventAgent::new(
            db.clone(),
            &config,
            notifier,
        )));
    }
    let mut schedules = ScheduleHandler::with_jev(db.clone(), planner, jev_client.clone());
    schedules = schedules.with_outbound(outbound.clone());
    let ticker = ScheduleTicker::new(db.clone());

    let summarizer = Arc::new(GeminiSummarizer::new(&config));
    let summaries = SummaryHandler::with_jev(db.clone(), summarizer, jev_client.clone());
    let task_executor = TaskExecutorHandler::new(db.clone(), &config);
    let wa_sweeper = WhatsAppSweeper::new(db.clone());

    let worker_id = Uuid::new_v4().to_string();
    let assigned_cancellation = cancellation.clone();
    let assigned_worker_id = worker_id.clone();
    let assigned_handle = tokio::spawn(async move {
        task_executor
            .run(&assigned_worker_id, assigned_cancellation)
            .await
    });
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
    let space_runtime = Arc::new(vox_core::agents::space_runtime::SpaceRuntime::new(
        db.clone(),
        &config,
        None,
    ));
    let mut worker = Worker::with_all_handlers(
        JobRepository::new(db),
        events,
        schedules,
        ticker,
        summaries,
        wa_sweeper,
        worker_id,
    );
    worker = worker.with_space_runtime(space_runtime);

    tokio::spawn(memory.run_greeting_sync(cancellation.clone()));

    let result = worker.run(cancellation.clone()).await;
    cancellation.cancel();
    assigned_handle.await?;
    connections_handle.await?;
    retention_handle.await?;
    att_handle.await?;
    gmail_handle.await?;
    pulse_handle.await?;
    if let Some(handle) = status_handle {
        handle.await?;
    }
    result?;
    Ok(())
}
