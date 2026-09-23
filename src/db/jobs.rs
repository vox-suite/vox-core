/**
* PostgreSQL job queue querying and transactional claiming helpers.
*/
use super::Db;
use crate::jobs::{ClaimedJob, JobKind};
use chrono::{DateTime, Duration, Utc};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone)]
pub struct JobRepository {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("job storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("job has an unknown kind")]
    UnknownKind,
    #[error("job lease is no longer owned")]
    LeaseLost,
}

impl JobRepository {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn enqueue(
        &self,
        kind: JobKind,
        payload_reference_id: Uuid,
    ) -> Result<Uuid, JobError> {
        sqlx::query_scalar(
            "INSERT INTO jobs (kind, payload_reference_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(kind.as_str())
        .bind(payload_reference_id)
        .fetch_one(self.db.pool())
        .await
        .map_err(Into::into)
    }

    pub async fn claim(
        &self,
        worker_id: &str,
        now: DateTime<Utc>,
        lease_duration: Duration,
        limit: i64,
    ) -> Result<Vec<ClaimedJob>, JobError> {
        let lease_expires_at = now + lease_duration;
        let rows = sqlx::query(
            "WITH candidates AS (\
                SELECT id FROM jobs \
                WHERE (state = 'pending' AND available_at <= $1) \
                   OR (state = 'running' AND lease_expires_at <= $1) \
                ORDER BY available_at, created_at \
                FOR UPDATE SKIP LOCKED LIMIT $2\
             ) \
             UPDATE jobs SET state = 'running', attempt_count = attempt_count + 1, \
                 lease_owner = $3, lease_expires_at = $4 \
             FROM candidates WHERE jobs.id = candidates.id \
             RETURNING jobs.id, jobs.kind, jobs.payload_reference_id, jobs.occurrence_at, jobs.attempt_count",
        )
        .bind(now)
        .bind(limit)
        .bind(worker_id)
        .bind(lease_expires_at)
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let kind: String = row.get("kind");
                Ok(ClaimedJob {
                    id: row.get("id"),
                    kind: JobKind::parse(&kind).ok_or(JobError::UnknownKind)?,
                    payload_reference_id: row.get("payload_reference_id"),
                    occurrence_at: row.get("occurrence_at"),
                    attempt_count: row.get("attempt_count"),
                })
            })
            .collect()
    }

    pub async fn complete(
        &self,
        job_id: Uuid,
        worker_id: &str,
        completed_at: DateTime<Utc>,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'completed', completed_at = $3, lease_owner = NULL, \
             lease_expires_at = NULL WHERE id = $1 AND state = 'running' AND lease_owner = $2",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(completed_at)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }

    pub async fn retry(
        &self,
        job_id: Uuid,
        worker_id: &str,
        next_attempt_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'pending', available_at = $3, last_error_code = $4, \
             lease_owner = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND state = 'running' AND lease_owner = $2",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(next_attempt_at)
        .bind(error_code)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }

    pub async fn fail(
        &self,
        job_id: Uuid,
        worker_id: &str,
        completed_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'failed', completed_at = $3, last_error_code = $4, \
             lease_owner = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND state = 'running' AND lease_owner = $2",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(completed_at)
        .bind(error_code)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }
}
