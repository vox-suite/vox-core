/**
* Background worker loops and asynchronous job processors.
*/
pub mod task_executor;
pub mod whatsapp_sweeper;

use crate::{
    conversations::ConversationId,
    db::jobs::JobRepository,
    events::{EventId, handler::EventHandler},
    jobs::JobKind,
    schedules::{ScheduleId, handler::ScheduleHandler, ticker::ScheduleTicker},
    sms_ingestion::retention::SmsRetentionSweeper,
    summaries::handler::SummaryHandler,
};
use chrono::{Duration, Utc};
use futures_util::{StreamExt, stream::FuturesUnordered};
use tokio_util::sync::CancellationToken;
use whatsapp_sweeper::WhatsAppSweeper;

pub struct Worker {
    jobs: JobRepository,
    events: EventHandler,
    schedules: Option<ScheduleHandler>,
    ticker: Option<ScheduleTicker>,
    summaries: Option<SummaryHandler>,
    wa_sweeper: Option<WhatsAppSweeper>,
    sms_retention: Option<SmsRetentionSweeper>,
    space_runtime: Option<std::sync::Arc<crate::agents::space_runtime::SpaceRuntime>>,
    worker_id: String,
}

impl Worker {
    pub fn new(jobs: JobRepository, events: EventHandler, worker_id: String) -> Self {
        Self {
            jobs,
            events,
            schedules: None,
            ticker: None,
            summaries: None,
            wa_sweeper: None,
            sms_retention: None,
            space_runtime: None,
            worker_id,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn with_all_handlers(
        jobs: JobRepository,
        events: EventHandler,
        schedules: ScheduleHandler,
        ticker: ScheduleTicker,
        summaries: SummaryHandler,
        wa_sweeper: WhatsAppSweeper,
        sms_retention: SmsRetentionSweeper,
        worker_id: String,
    ) -> Self {
        Self {
            jobs,
            events,
            schedules: Some(schedules),
            ticker: Some(ticker),
            summaries: Some(summaries),
            wa_sweeper: Some(wa_sweeper),
            sms_retention: Some(sms_retention),
            space_runtime: None,
            worker_id,
        }
    }

    pub fn with_space_runtime(
        mut self,
        runner: std::sync::Arc<crate::agents::space_runtime::SpaceRuntime>,
    ) -> Self {
        self.space_runtime = Some(runner);
        self
    }

    pub async fn run(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), crate::db::jobs::JobError> {
        tokio::try_join!(
            self.run_jobs(cancellation.clone()),
            self.run_maintenance(cancellation),
        )?;
        Ok(())
    }

    const JOB_CLAIM_BATCH: i64 = 20;
    const JOB_CONCURRENCY: usize = 8;

    async fn handle_one(
        &self,
        job: crate::jobs::ClaimedJob,
    ) -> Result<(), crate::db::jobs::JobError> {
        tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job claimed");
        let Some(reference_id) = job.payload_reference_id else {
            tracing::error!(job_id = %job.id, kind = job.kind.as_str(), "job has no payload_reference_id; failing");
            self.jobs
                .fail(
                    job.id,
                    &self.worker_id,
                    Utc::now(),
                    "missing_payload_reference_id",
                )
                .await?;
            return Ok(());
        };
        let heartbeat = {
            let jobs = self.jobs.clone();
            let worker_id = self.worker_id.clone();
            let job_id = job.id;
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                    let until = Utc::now() + Duration::seconds(30);
                    if jobs.extend_lease(job_id, &worker_id, until).await.is_err() {
                        break;
                    }
                }
            })
        };
        let result = match job.kind {
            JobKind::ProcessEvent => self
                .events
                .handle(EventId(reference_id))
                .await
                .map_err(|_| "event_processing"),
            JobKind::ProcessEventBatch => self
                .events
                .handle_batch(reference_id)
                .await
                .map_err(|_| "event_batch_processing"),
            JobKind::RunSchedule => match &self.schedules {
                Some(schedules) => match job.occurrence_at {
                    Some(occurrence_at) => schedules
                        .handle(ScheduleId(reference_id), occurrence_at)
                        .await
                        .map_err(|_| "schedule_processing"),
                    None => Err("schedule_occurrence_missing"),
                },
                None => Err("schedule_handler_unavailable"),
            },
            JobKind::SummarizeConversation => match &self.summaries {
                Some(summaries) => summaries
                    .handle(ConversationId(reference_id))
                    .await
                    .map_err(|_| "summary_processing"),
                None => Err("summary_handler_unavailable"),
            },
            JobKind::EvaluateSpan | JobKind::ExecuteSpan => Err("assigned_run_binding_required"),
            JobKind::RunSpace => match &self.space_runtime {
                Some(runner) => runner
                    .run_space(reference_id, None)
                    .await
                    .map_err(|_| "space_execution"),
                None => Err("space_runtime_unavailable"),
            },
        };

        heartbeat.abort();
        match result {
            Ok(()) => {
                tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job completed");
                self.jobs
                    .complete(job.id, &self.worker_id, Utc::now())
                    .await?
            }
            Err(code) if job.attempt_count >= job.max_attempts => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, "job failed permanently");
                self.jobs
                    .fail(job.id, &self.worker_id, Utc::now(), code)
                    .await?
            }
            Err(code) => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, attempt = job.attempt_count, "job failed, retrying");
                let seconds = 2_i64.pow(job.attempt_count.clamp(1, 6) as u32);
                self.jobs
                    .retry(
                        job.id,
                        &self.worker_id,
                        Utc::now() + Duration::seconds(seconds),
                        code,
                    )
                    .await?;
            }
        }
        Ok(())
    }

    async fn run_jobs(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), crate::db::jobs::JobError> {
        let mut inflight = FuturesUnordered::new();
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = interval.tick() => {}
                Some(result) = inflight.next(), if !inflight.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "job handling failed");
                    }
                }
            }
            let free = Self::JOB_CONCURRENCY - inflight.len();
            if free == 0 {
                continue;
            }
            let jobs = self
                .jobs
                .claim(
                    &self.worker_id,
                    Utc::now(),
                    Duration::seconds(30),
                    (free as i64).min(Self::JOB_CLAIM_BATCH),
                )
                .await?;
            for job in jobs {
                inflight.push(self.handle_one(job));
            }
        }
    }

    async fn run_maintenance(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), crate::db::jobs::JobError> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = interval.tick() => {}
            }
            if let Some(ticker) = &self.ticker
                && let Err(error) = ticker.tick(Utc::now()).await
            {
                tracing::warn!(%error, "schedule ticker failed");
            }
            if let Some(sweeper) = &self.wa_sweeper
                && let Err(error) = sweeper.sweep_inactive_conversations().await
            {
                tracing::warn!(%error, "whatsapp sweeper failed");
            }
            match self.events.requeue_failed().await {
                Ok(0) => {}
                Ok(count) => tracing::info!(count, "requeued failed events"),
                Err(error) => tracing::warn!(%error, "event requeue sweeper failed"),
            }
            if let Some(sweeper) = &self.sms_retention
                && let Err(error) = sweeper.purge_expired().await
            {
                tracing::warn!(%error, "sms retention sweeper failed");
            }
        }
    }
}
