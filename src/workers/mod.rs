use crate::{
    actions::{ActionId, handler::ActionHandler},
    conversations::ConversationId,
    db::jobs::JobRepository,
    events::{EventId, handler::EventHandler},
    jobs::JobKind,
    schedules::{ScheduleId, handler::ScheduleHandler, ticker::ScheduleTicker},
    summaries::handler::SummaryHandler,
};
use chrono::{Duration, Utc};
use tokio_util::sync::CancellationToken;

pub struct Worker {
    jobs: JobRepository,
    events: EventHandler,
    schedules: Option<ScheduleHandler>,
    ticker: Option<ScheduleTicker>,
    actions: Option<ActionHandler>,
    summaries: Option<SummaryHandler>,
    worker_id: String,
}

impl Worker {
    pub fn new(jobs: JobRepository, events: EventHandler, worker_id: String) -> Self {
        Self {
            jobs,
            events,
            schedules: None,
            ticker: None,
            actions: None,
            summaries: None,
            worker_id,
        }
    }

    pub fn with_all_handlers(
        jobs: JobRepository,
        events: EventHandler,
        schedules: ScheduleHandler,
        ticker: ScheduleTicker,
        actions: ActionHandler,
        summaries: SummaryHandler,
        worker_id: String,
    ) -> Self {
        Self {
            jobs,
            events,
            schedules: Some(schedules),
            ticker: Some(ticker),
            actions: Some(actions),
            summaries: Some(summaries),
            worker_id,
        }
    }

    pub async fn run(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), crate::db::jobs::JobError> {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            tokio::select! {
                _ = cancellation.cancelled() => return Ok(()),
                _ = interval.tick() => self.run_once().await?,
            }
        }
    }

    async fn run_once(&self) -> Result<(), crate::db::jobs::JobError> {
        let now = Utc::now();
        if let Some(ticker) = &self.ticker {
            let _ = ticker.tick(now).await;
        }

        let jobs = self
            .jobs
            .claim(&self.worker_id, now, Duration::seconds(30), 10)
            .await?;

        for job in jobs {
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
                JobKind::DispatchAction => match &self.actions {
                    Some(actions) => actions
                        .handle(ActionId(job.payload_reference_id))
                        .await
                        .map_err(|_| "action_dispatch"),
                    None => Err("action_handler_unavailable"),
                },
                JobKind::SummarizeConversation => match &self.summaries {
                    Some(summaries) => summaries
                        .handle(ConversationId(job.payload_reference_id))
                        .await
                        .map_err(|_| "summary_processing"),
                    None => Err("summary_handler_unavailable"),
                },
            };

            match result {
                Ok(()) => {
                    self.jobs
                        .complete(job.id, &self.worker_id, Utc::now())
                        .await?
                }
                Err(code) if job.attempt_count >= 5 => {
                    self.jobs
                        .fail(job.id, &self.worker_id, Utc::now(), code)
                        .await?
                }
                Err(code) => {
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
