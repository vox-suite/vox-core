pub mod handler;
pub mod retention;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    consent::{ConsentError, ConsentService, DataSource},
    db::{Db, jobs::JobRepository},
    jobs::JobKind,
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SmsMessage {
    pub sender: String,
    pub body: String,
    pub received_at: DateTime<Utc>,
}

#[derive(Debug, thiserror::Error)]
pub enum SmsIngestionError {
    #[error("batch must contain at least one message")]
    Empty,
    #[error("sms data sharing consent has not been granted")]
    ConsentRequired,
    #[error("sms batch storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("job queue unavailable")]
    Job(#[from] crate::db::jobs::JobError),
    #[error("consent storage unavailable")]
    Consent(#[from] ConsentError),
}

#[derive(Clone)]
pub struct SmsIngestionService {
    db: Db,
    jobs: JobRepository,
    consent: ConsentService,
}

impl SmsIngestionService {
    pub fn new(db: Db) -> Self {
        let jobs = JobRepository::new(db.clone());
        let consent = ConsentService::new(db.clone());
        Self { db, jobs, consent }
    }

    pub async fn submit_batch(
        &self,
        user_id: Uuid,
        messages: Vec<SmsMessage>,
    ) -> Result<Uuid, SmsIngestionError> {
        if messages.is_empty() {
            return Err(SmsIngestionError::Empty);
        }
        if !self.consent.is_granted(user_id, DataSource::Sms).await? {
            return Err(SmsIngestionError::ConsentRequired);
        }

        let payload = serde_json::to_value(&messages).unwrap_or_else(|_| serde_json::json!([]));
        let batch_id: Uuid = sqlx::query_scalar(
            "INSERT INTO sms_batches (user_id, messages) VALUES ($1, $2) RETURNING id",
        )
        .bind(user_id)
        .bind(payload)
        .fetch_one(self.db.pool())
        .await?;

        self.jobs
            .enqueue(JobKind::ProcessSmsBatch, batch_id)
            .await?;
        tracing::info!(%user_id, %batch_id, message_count = messages.len(), "sms batch received and enqueued");

        let newest_received_at = messages.iter().map(|m| m.received_at).max().unwrap();
        self.consent
            .advance_sync_cursor(user_id, DataSource::Sms, newest_received_at)
            .await?;

        Ok(batch_id)
    }
}
