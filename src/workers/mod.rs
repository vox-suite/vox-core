use crate::{
    db::jobs::JobRepository,
    events::{EventId, handler::EventHandler},
    jobs::JobKind,
};
use chrono::{Duration, Utc};
use tokio_util::sync::CancellationToken;

pub struct Worker {
    jobs: JobRepository,
    events: EventHandler,
    worker_id: String,
}

impl Worker {
    pub fn new(jobs: JobRepository, events: EventHandler, worker_id: String) -> Self {
        Self {
            jobs,
            events,
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
                _ => Err("handler_unavailable"),
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
