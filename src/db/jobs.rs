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
        self.enqueue_with_priority(kind, payload_reference_id, 0)
            .await
    }

    pub async fn enqueue_with_priority(
        &self,
        kind: JobKind,
        payload_reference_id: Uuid,
        priority: i16,
    ) -> Result<Uuid, JobError> {
        sqlx::query_scalar(
            "INSERT INTO jobs (kind, payload_reference_id, priority) VALUES ($1, $2, $3) RETURNING id",
        )
        .bind(kind.as_str())
        .bind(payload_reference_id)
        .bind(priority)
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
                WHERE wait_reason IS NULL AND kind IN ('process_event','process_event_batch','run_schedule','summarize_conversation','run_space') \
                  AND ((state = 'pending' AND available_at <= $1) \
                   OR (state = 'running' AND lease_expires_at <= $1)) \
                ORDER BY priority DESC, available_at, created_at \
                FOR UPDATE SKIP LOCKED LIMIT $2\
             ) \
             UPDATE jobs SET state = 'running', attempt_count = attempt_count + 1, \
                 lease_owner = $3, lease_expires_at = $4, lease_generation = lease_generation + 1 \
             FROM candidates WHERE jobs.id = candidates.id \
             RETURNING jobs.id, jobs.kind, jobs.payload_reference_id, jobs.occurrence_at, jobs.attempt_count, jobs.max_attempts, jobs.lease_generation",
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
                    max_attempts: row.get("max_attempts"),
                    lease_generation: row.get("lease_generation"),
                })
            })
            .collect()
    }

    pub async fn extend_lease(
        &self,
        job_id: Uuid,
        worker_id: &str,
        lease_generation: i64,
        lease_expires_at: DateTime<Utc>,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET lease_expires_at = $3 \
             WHERE id = $1 AND state = 'running' AND lease_owner = $2 AND lease_generation = $4 AND lease_expires_at > now()",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(lease_expires_at)
        .bind(lease_generation)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }

    pub async fn complete(
        &self,
        job_id: Uuid,
        worker_id: &str,
        lease_generation: i64,
        completed_at: DateTime<Utc>,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'completed', completed_at = $3, lease_owner = NULL, \
             lease_expires_at = NULL WHERE id = $1 AND state = 'running' AND lease_owner = $2 AND lease_generation = $4 AND lease_expires_at > now()",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(completed_at)
        .bind(lease_generation)
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
        lease_generation: i64,
        next_attempt_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'pending', available_at = $3, last_error_code = $4, \
             lease_owner = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND state = 'running' AND lease_owner = $2 AND lease_generation = $5 AND lease_expires_at > now()",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(next_attempt_at)
        .bind(error_code)
        .bind(lease_generation)
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
        lease_generation: i64,
        completed_at: DateTime<Utc>,
        error_code: &str,
    ) -> Result<(), JobError> {
        let result = sqlx::query(
            "UPDATE jobs SET state = 'failed', completed_at = $3, last_error_code = $4, \
             lease_owner = NULL, lease_expires_at = NULL \
             WHERE id = $1 AND state = 'running' AND lease_owner = $2 AND lease_generation = $5 AND lease_expires_at > now()",
        )
        .bind(job_id)
        .bind(worker_id)
        .bind(completed_at)
        .bind(error_code)
        .bind(lease_generation)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(JobError::LeaseLost)
        }
    }
}

#[cfg(test)]
mod lease_regression_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn expired_job_cannot_be_renewed_or_completed() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let jobs = JobRepository::new(db.clone());
        let id = jobs
            .enqueue(JobKind::ProcessEvent, Uuid::new_v4())
            .await
            .unwrap();
        let owner = Uuid::new_v4().to_string();
        let claimed = jobs
            .claim(&owner, Utc::now(), Duration::seconds(30), 100)
            .await
            .unwrap();
        assert!(claimed.iter().any(|job| job.id == id));
        let generation = claimed
            .iter()
            .find(|job| job.id == id)
            .unwrap()
            .lease_generation;
        sqlx::query("UPDATE jobs SET lease_expires_at=now()-interval '1 second' WHERE id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
        assert!(matches!(
            jobs.extend_lease(id, &owner, generation, Utc::now() + Duration::seconds(30))
                .await,
            Err(JobError::LeaseLost)
        ));
        assert!(matches!(
            jobs.complete(id, &owner, generation, Utc::now()).await,
            Err(JobError::LeaseLost)
        ));
        sqlx::query("DELETE FROM jobs WHERE id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn reclaimed_job_rejects_previous_generation_even_with_same_owner() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let jobs = JobRepository::new(db.clone());
        let id = jobs
            .enqueue_with_priority(JobKind::ProcessEvent, Uuid::new_v4(), i16::MAX)
            .await
            .unwrap();
        let owner = Uuid::new_v4().to_string();
        let first = jobs
            .claim(&owner, Utc::now(), Duration::seconds(30), 100)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.id == id)
            .unwrap();
        sqlx::query("UPDATE jobs SET lease_expires_at=now()-interval '1 second' WHERE id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
        let second = jobs
            .claim(&owner, Utc::now(), Duration::seconds(30), 100)
            .await
            .unwrap()
            .into_iter()
            .find(|j| j.id == id)
            .unwrap();
        assert!(second.lease_generation > first.lease_generation);
        assert!(matches!(
            jobs.extend_lease(
                id,
                &owner,
                first.lease_generation,
                Utc::now() + Duration::seconds(30)
            )
            .await,
            Err(JobError::LeaseLost)
        ));
        assert!(matches!(
            jobs.complete(id, &owner, first.lease_generation, Utc::now())
                .await,
            Err(JobError::LeaseLost)
        ));
        assert!(matches!(
            jobs.retry(id, &owner, first.lease_generation, Utc::now(), "stale")
                .await,
            Err(JobError::LeaseLost)
        ));
        assert!(matches!(
            jobs.fail(id, &owner, first.lease_generation, Utc::now(), "stale")
                .await,
            Err(JobError::LeaseLost)
        ));
        jobs.extend_lease(
            id,
            &owner,
            second.lease_generation,
            Utc::now() + Duration::seconds(30),
        )
        .await
        .unwrap();
        jobs.complete(id, &owner, second.lease_generation, Utc::now())
            .await
            .unwrap();
        sqlx::query("DELETE FROM jobs WHERE id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
    #[tokio::test]
    #[ignore = "requires isolated TEST_DATABASE_URL"]
    async fn general_worker_does_not_claim_assigned_span_jobs() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let jobs = JobRepository::new(db.clone());
        let id = jobs
            .enqueue_with_priority(JobKind::ExecuteSpan, Uuid::new_v4(), i16::MAX)
            .await
            .unwrap();
        let claimed = jobs
            .claim("general-regression", Utc::now(), Duration::seconds(30), 100)
            .await
            .unwrap();
        assert!(!claimed.iter().any(|job| job.id == id));
        sqlx::query("DELETE FROM jobs WHERE id=$1")
            .bind(id)
            .execute(db.pool())
            .await
            .unwrap();
    }
}
