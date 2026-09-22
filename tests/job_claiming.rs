/**
* Integration tests for transactional background job claiming.
*/
use chrono::{Duration, Utc};
use uuid::Uuid;
use vox_core::{
    db::{Db, jobs::JobRepository},
    jobs::JobKind,
};

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn claims_once_recovers_expired_leases_and_never_reclaims_completion() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    sqlx::query("TRUNCATE jobs CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let repository = JobRepository::new(db);
    let job_id = repository
        .enqueue(JobKind::ProcessEvent, Uuid::new_v4())
        .await
        .unwrap();
    let now = Utc::now();

    let (first, second) = tokio::join!(
        repository.claim("worker-a", now, Duration::seconds(30), 1),
        repository.claim("worker-b", now, Duration::seconds(30), 1),
    );
    let mut claims = first.unwrap();
    claims.extend(second.unwrap());
    assert_eq!(claims.len(), 1);
    assert_eq!(claims[0].id, job_id);

    let recovered = repository
        .claim(
            "worker-c",
            now + Duration::seconds(31),
            Duration::seconds(30),
            1,
        )
        .await
        .unwrap();
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].id, job_id);
    repository
        .complete(job_id, "worker-c", now + Duration::seconds(32))
        .await
        .unwrap();

    let after_completion = repository
        .claim(
            "worker-d",
            now + Duration::minutes(2),
            Duration::seconds(30),
            1,
        )
        .await
        .unwrap();
    assert!(after_completion.is_empty());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn retries_after_backoff_and_terminal_failures_stay_finished() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE jobs CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let repository = JobRepository::new(db);
    let job_id = repository
        .enqueue(JobKind::ProcessEvent, Uuid::new_v4())
        .await
        .unwrap();
    let now = Utc::now();
    repository
        .claim("worker-a", now, Duration::seconds(30), 1)
        .await
        .unwrap();
    repository
        .retry(job_id, "worker-a", now + Duration::seconds(10), "temporary")
        .await
        .unwrap();
    assert!(
        repository
            .claim(
                "worker-b",
                now + Duration::seconds(9),
                Duration::seconds(30),
                1
            )
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        repository
            .claim(
                "worker-b",
                now + Duration::seconds(10),
                Duration::seconds(30),
                1
            )
            .await
            .unwrap()
            .len(),
        1
    );
    repository
        .fail(job_id, "worker-b", now + Duration::seconds(11), "exhausted")
        .await
        .unwrap();
    assert!(
        repository
            .claim(
                "worker-c",
                now + Duration::minutes(1),
                Duration::seconds(30),
                1
            )
            .await
            .unwrap()
            .is_empty()
    );
}
