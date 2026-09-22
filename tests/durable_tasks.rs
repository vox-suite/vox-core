/**
* Integration tests for durable task scheduling and execution.
*/
use chrono::{Duration, Utc};
use uuid::Uuid;
use vox_core::{
    db::Db,
    durable_tasks::{DurableTaskService, RunState, StartTaskRequest, WaitReason, WaitRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
};

async fn setup() -> Db {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db
}

async fn context(db: &Db) -> vox_core::identity::ResolvedUserContext {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("durable-{}", Uuid::new_v4()),
            host_app_external_key: "host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: "user".into(),
        organization_external_key: None,
    };
    let now = Utc::now();
    let assertion = host
        .credential
        .sign_context_request(&request, now, Uuid::new_v4())
        .unwrap();
    trust
        .resolve_authenticated_context(&assertion, &request, None, now)
        .await
        .unwrap()
}

fn request() -> StartTaskRequest {
    StartTaskRequest {
        title: "Book a table".into(),
        instruction: "Find options and wait for my approval".into(),
        agent_external_key: None,
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn tasks_survive_restart_wait_without_client_lifecycle_and_cancel_future_work() {
    let db = setup().await;
    let context = context(&db).await;
    let tasks = DurableTaskService::new(db);
    let created = tasks.start(&context, request()).await.unwrap();
    assert_eq!(
        tasks.get(&context, created.id).await.unwrap().state,
        RunState::Queued,
        "a client disconnect has no task transition"
    );
    let now = Utc::now();
    let claimed = tasks
        .claim_next("worker-a", now, Duration::seconds(1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claimed.task.id, created.id);
    assert_eq!(claimed.task.state, RunState::Running);
    assert_eq!(
        tasks
            .recover_expired(now + Duration::seconds(2))
            .await
            .unwrap(),
        1,
        "restart recovery returns expired work to durable queue"
    );
    let re_claimed = tasks
        .claim_next(
            "worker-b",
            now + Duration::seconds(3),
            Duration::seconds(30),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(re_claimed.task.id, created.id);
    let waiting = tasks
        .wait(
            &context,
            created.id,
            WaitRequest {
                reason: WaitReason::Approval,
                checkpoint: serde_json::json!({"proposal":"p-1"}),
            },
        )
        .await
        .unwrap();
    assert_eq!(waiting.state, RunState::Waiting);
    assert_eq!(waiting.wait_reason, Some(WaitReason::Approval));
    assert_eq!(
        tasks
            .recover_expired(now + Duration::hours(1))
            .await
            .unwrap(),
        0,
        "waiting work is not retried because a client disconnected or a worker restarted"
    );
    assert_eq!(
        tasks.get(&context, created.id).await.unwrap().wait_reason,
        Some(WaitReason::Approval)
    );
    tasks.resume(&context, created.id).await.unwrap();
    let cancelled = tasks.cancel(&context, created.id).await.unwrap();
    assert_eq!(cancelled.state, RunState::Cancelled);
    assert!(
        tasks
            .claim_next("worker-c", Utc::now(), Duration::seconds(30))
            .await
            .unwrap()
            .is_none(),
        "cancellation stops future work without claiming undo"
    );
}
