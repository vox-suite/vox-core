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
    sms_ingestion::{handler::SmsBatchHandler, retention::SmsRetentionSweeper},
    summaries::handler::SummaryHandler,
};
use chrono::{Duration, Utc};
use task_executor::TaskExecutorHandler;
use tokio_util::sync::CancellationToken;
use whatsapp_sweeper::WhatsAppSweeper;

pub struct Worker {
    jobs: JobRepository,
    events: EventHandler,
    schedules: Option<ScheduleHandler>,
    ticker: Option<ScheduleTicker>,
    summaries: Option<SummaryHandler>,
    task_executor: Option<TaskExecutorHandler>,
    wa_sweeper: Option<WhatsAppSweeper>,
    sms_batches: Option<SmsBatchHandler>,
    sms_retention: Option<SmsRetentionSweeper>,
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
            task_executor: None,
            wa_sweeper: None,
            sms_batches: None,
            sms_retention: None,
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
        task_executor: TaskExecutorHandler,
        wa_sweeper: WhatsAppSweeper,
        sms_batches: SmsBatchHandler,
        sms_retention: SmsRetentionSweeper,
        worker_id: String,
    ) -> Self {
        Self {
            jobs,
            events,
            schedules: Some(schedules),
            ticker: Some(ticker),
            summaries: Some(summaries),
            task_executor: Some(task_executor),
            wa_sweeper: Some(wa_sweeper),
            sms_batches: Some(sms_batches),
            sms_retention: Some(sms_retention),
            worker_id,
        }
    }

    pub async fn run(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), crate::db::jobs::JobError> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = interval.tick() => self.run_once().await?,
            }
        }
    }

    async fn run_once(&self) -> Result<(), crate::db::jobs::JobError> {
        let now = Utc::now();
        if let Some(ticker) = &self.ticker
            && let Err(error) = ticker.tick(now).await
        {
            tracing::warn!(%error, "schedule ticker failed");
        }

        if let Some(sweeper) = &self.wa_sweeper
            && let Err(error) = sweeper.sweep_inactive_conversations().await
        {
            tracing::warn!(%error, "whatsapp sweeper failed");
        }

        if let Some(sweeper) = &self.sms_retention
            && let Err(error) = sweeper.purge_expired().await
        {
            tracing::warn!(%error, "sms retention sweeper failed");
        }

        let jobs = self
            .jobs
            .claim(&self.worker_id, now, Duration::seconds(30), 10)
            .await?;

        for job in jobs {
            tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job claimed");
            let result = match job.kind {
                JobKind::ProcessEvent => self
                    .events
                    .handle(EventId(job.payload_reference_id))
                    .await
                    .map_err(|_| "event_processing"),
                JobKind::RunSchedule => match &self.schedules {
                    Some(schedules) => match job.occurrence_at {
                        Some(occurrence_at) => schedules
                            .handle(ScheduleId(job.payload_reference_id), occurrence_at)
                            .await
                            .map_err(|_| "schedule_processing"),
                        None => Err("schedule_occurrence_missing"),
                    },
                    None => Err("schedule_handler_unavailable"),
                },
                JobKind::SummarizeConversation => match &self.summaries {
                    Some(summaries) => summaries
                        .handle(ConversationId(job.payload_reference_id))
                        .await
                        .map_err(|_| "summary_processing"),
                    None => Err("summary_handler_unavailable"),
                },
                JobKind::EvaluateSpan | JobKind::ExecuteSpan => match &self.task_executor {
                    Some(executor) => executor
                        .handle(job.payload_reference_id)
                        .await
                        .map_err(|_| "task_execution"),
                    None => Err("task_executor_unavailable"),
                },
                JobKind::ProcessSmsBatch => match &self.sms_batches {
                    Some(handler) => handler
                        .handle(job.payload_reference_id)
                        .await
                        .map_err(|_| "sms_batch_processing"),
                    None => Err("sms_batch_handler_unavailable"),
                },
            };

            match result {
                Ok(()) => {
                    tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job completed");
                    self.jobs
                        .complete(job.id, &self.worker_id, Utc::now())
                        .await?
                }
                Err(code) if job.attempt_count >= 5 => {
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
        }
        Ok(())
    }
}
