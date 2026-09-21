//! Isolated PostgreSQL acceptance coverage for the status cursor and webhook
//! subscription lifecycle. Run with `TEST_DATABASE_URL=... cargo test --locked
//! --test status_updates -- --ignored --test-threads=1`.

use async_trait::async_trait;
use chrono::Utc;
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;
use vox_core::{
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    status::{
        AppendStatusEvent, CreateSubscriptionRequest, StatusError, StatusService,
        WebhookSecretStore,
    },
};

#[derive(Default)]
struct TestSecretStore(Mutex<HashMap<Uuid, String>>);

#[async_trait]
impl WebhookSecretStore for TestSecretStore {
    async fn put(&self, id: Uuid, secret: String) -> Result<(), StatusError> {
        self.0.lock().unwrap().insert(id, secret);
        Ok(())
    }
    async fn get(&self, id: Uuid) -> Result<String, StatusError> {
        self.0
            .lock()
            .unwrap()
            .get(&id)
            .cloned()
            .ok_or(StatusError::NotFound)
    }
    async fn delete(&self, id: Uuid) -> Result<(), StatusError> {
        self.0.lock().unwrap().remove(&id);
        Ok(())
    }
}

async fn setup_context(db: &Db) -> vox_core::identity::ResolvedUserContext {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("status-test-{}", Uuid::new_v4()),
            host_app_external_key: "status-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: "status-user".into(),
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn cursors_are_scoped_replayable_and_subscription_secrets_are_one_time() {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let owner = setup_context(&db).await;
    let other = setup_context(&db).await;
    let secrets = std::sync::Arc::new(TestSecretStore::default());
    let status = StatusService::new(db.clone()).with_secret_store(secrets.clone());

    // Normal durable-task writes atomically create task and run status events.
    let task = DurableTaskService::new(db)
        .start(
            &owner,
            StartTaskRequest {
                title: "Status transition".into(),
                instruction: "Verify atomic status events".into(),
                agent_external_key: None,
            },
        )
        .await
        .unwrap();

    let first = status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "queued".into(),
                deduplication_key: "status-first".into(),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let duplicate = status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: first.aggregate_id,
                event_type: "task.state_changed".into(),
                state: "queued".into(),
                deduplication_key: "status-first".into(),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert_eq!(first.cursor, duplicate.cursor);
    assert_eq!(status.list(&other, 0, 100).await.unwrap().len(), 0);
    let events = status.list(&owner, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.cursor == first.cursor));
    assert!(
        events
            .iter()
            .any(|event| event.aggregate_id == task.id && event.aggregate_type == "task")
    );
    assert!(
        events
            .iter()
            .any(|event| event.aggregate_id == task.run_id && event.aggregate_type == "run")
    );

    let created = status
        .create_subscription(
            &owner,
            CreateSubscriptionRequest {
                endpoint: "https://host.example/vox-status".into(),
            },
        )
        .await
        .unwrap();
    assert!(created.secret.is_some());
    assert_eq!(
        secrets.get(created.id).await.unwrap(),
        created.secret.clone().unwrap()
    );
    assert!(
        status.list_subscriptions(&owner).await.unwrap()[0]
            .secret
            .is_none()
    );
    let rotated = status
        .rotate_subscription(&owner, created.id)
        .await
        .unwrap();
    assert_ne!(created.secret, rotated.secret);
    status
        .disable_subscription(&owner, created.id)
        .await
        .unwrap();
    assert_eq!(
        status.list_subscriptions(&owner).await.unwrap()[0].state,
        "disabled"
    );
}
