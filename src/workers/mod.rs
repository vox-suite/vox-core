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
    space_runtime: Option<std::sync::Arc<crate::agents::space_runtime::SpaceRuntime>>,
    worker_id: String,
}

// A cancelled worker future must not leave an independent task renewing its lease.
struct LeaseHeartbeat {
    task: tokio::task::JoinHandle<()>,
}
impl LeaseHeartbeat {
    async fn run<T>(
        mut self,
        handler: impl std::future::Future<Output = T>,
    ) -> Result<T, crate::db::jobs::JobError> {
        tokio::select! {
            biased;
            _ = &mut self.task => Err(crate::db::jobs::JobError::LeaseLost),
            result = handler => Ok(result),
        }
    }
}
impl Drop for LeaseHeartbeat {
    fn drop(&mut self) {
        self.task.abort();
    }
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
        worker_id: String,
    ) -> Self {
        Self {
            jobs,
            events,
            schedules: Some(schedules),
            ticker: Some(ticker),
            summaries: Some(summaries),
            wa_sweeper: Some(wa_sweeper),
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
                    job.lease_generation,
                    Utc::now(),
                    "missing_payload_reference_id",
                )
                .await?;
            return Ok(());
        };
        let heartbeat = LeaseHeartbeat {
            task: {
                let jobs = self.jobs.clone();
                let worker_id = self.worker_id.clone();
                let job_id = job.id;
                let generation = job.lease_generation;
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                        let until = Utc::now() + Duration::seconds(30);
                        match tokio::time::timeout(
                            std::time::Duration::from_secs(8),
                            jobs.extend_lease(job_id, &worker_id, generation, until),
                        )
                        .await
                        {
                            Ok(Ok(())) => {}
                            _ => return,
                        }
                    }
                })
            },
        };
        let handler = async {
            match job.kind {
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
                JobKind::EvaluateSpan | JobKind::ExecuteSpan => {
                    Err("assigned_run_binding_required")
                }
                JobKind::RunSpace => match &self.space_runtime {
                    Some(runner) => runner
                        .run_space(reference_id, None)
                        .await
                        .map_err(|_| "space_execution"),
                    None => Err("space_runtime_unavailable"),
                },
            }
        };
        let result = heartbeat.run(handler).await?;
        match result {
            Ok(()) => {
                tracing::info!(job_id = %job.id, kind = job.kind.as_str(), "job completed");
                self.jobs
                    .complete(job.id, &self.worker_id, job.lease_generation, Utc::now())
                    .await?
            }
            Err(code) if job.attempt_count >= job.max_attempts => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, "job failed permanently");
                self.jobs
                    .fail(
                        job.id,
                        &self.worker_id,
                        job.lease_generation,
                        Utc::now(),
                        code,
                    )
                    .await?
            }
            Err(code) => {
                tracing::warn!(job_id = %job.id, kind = job.kind.as_str(), code, attempt = job.attempt_count, "job failed, retrying");
                let seconds = 2_i64.pow(job.attempt_count.clamp(1, 6) as u32);
                self.jobs
                    .retry(
                        job.id,
                        &self.worker_id,
                        job.lease_generation,
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
        }
    }
}

#[cfg(test)]
mod lease_cancellation_tests {
    use super::*;
    struct Dropped(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Dropped {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    }
    #[tokio::test]
    async fn lost_heartbeat_cancels_pending_handler() {
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let heartbeat = LeaseHeartbeat {
            task: tokio::spawn(async {}),
        };
        let handler = async move {
            let _guard = guard;
            std::future::pending::<()>().await;
        };
        assert!(matches!(
            heartbeat.run(handler).await,
            Err(crate::db::jobs::JobError::LeaseLost)
        ));
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }
    #[tokio::test]
    async fn dropping_heartbeat_guard_aborts_renewal_task() {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Dropped(dropped.clone());
        let task = tokio::spawn(async move {
            let _guard = guard;
            started_tx.send(()).unwrap();
            std::future::pending::<()>().await;
        });
        let finished = task.abort_handle();
        let heartbeat = LeaseHeartbeat { task };
        started_rx.await.unwrap();
        drop(heartbeat);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !dropped.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(finished.is_finished());
    }
}
