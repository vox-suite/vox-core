/**
* Integration tests for task status update webhooks.
*/
use async_trait::async_trait;
use chrono::Utc;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use std::{collections::HashMap, sync::Mutex};
use uuid::Uuid;
use vox_core::{
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    status::{
        AppendStatusEvent, CreateSubscriptionRequest, EncryptedWebhookSecretStore, StatusError,
        StatusService, WebhookRequest, WebhookSecretStore, WebhookTransport,
    },
};

#[derive(Default)]
struct TestSecretStore(Mutex<HashMap<Uuid, String>>);

#[derive(Default)]
struct RecordingTransport(Mutex<Vec<WebhookRequest>>);

#[async_trait]
impl WebhookTransport for RecordingTransport {
    async fn post(&self, request: WebhookRequest) -> Option<u16> {
        self.0.lock().unwrap().push(request);
        Some(202)
    }
}

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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn encrypted_custody_and_transactional_outbox_survive_service_recreation() {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let owner = setup_context(&db).await;
    let key = "a1".repeat(32);
    let secrets =
        std::sync::Arc::new(EncryptedWebhookSecretStore::from_hex_key(db.clone(), &key).unwrap());
    let status = StatusService::new(db.clone()).with_secret_store(secrets.clone());
    let created = status
        .create_subscription(
            &owner,
            CreateSubscriptionRequest {
                endpoint: "https://host.example/vox-status".into(),
            },
        )
        .await
        .unwrap();
    let stored: Vec<u8> = sqlx::query_scalar(
        "SELECT ciphertext FROM status_webhook_secrets WHERE subscription_id=$1",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    let plain = created.secret.as_ref().unwrap();
    assert!(
        !stored
            .windows(plain.len())
            .any(|window| window == plain.as_bytes())
    );
    let reopened = EncryptedWebhookSecretStore::from_hex_key(db.clone(), &key).unwrap();
    assert_eq!(reopened.get(created.id).await.unwrap(), *plain);

    status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "queued".into(),
                deduplication_key: format!("outbox-{}", Uuid::new_v4()),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let queued: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM status_webhook_deliveries
         WHERE subscription_id=$1 AND state='queued'",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(queued, 1);

    let transport = std::sync::Arc::new(RecordingTransport::default());
    assert!(
        status
            .delivery_worker()
            .with_transport(transport.clone())
            .deliver_next("successful-worker", Utc::now())
            .await
            .unwrap()
    );
    {
        let sent = transport.0.lock().unwrap();
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].endpoint, "https://host.example/vox-status");
        let payload: serde_json::Value = serde_json::from_slice(&sent[0].body).unwrap();
        assert_eq!(payload["authoritative"], false);
        assert_eq!(payload["delivery_id"], sent[0].delivery_id.to_string());
        let mut mac = Hmac::<Sha256>::new_from_slice(plain.as_bytes()).unwrap();
        mac.update(sent[0].timestamp.as_bytes());
        mac.update(b".");
        mac.update(&sent[0].body);
        assert_eq!(sent[0].signature, hex::encode(mac.finalize().into_bytes()));
    }

    status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "running".into(),
                deduplication_key: format!("outbox-{}", Uuid::new_v4()),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();

    // A compromised DNS record or a later endpoint change must never make
    // the worker send a status hint to a loopback address. The durable row
    // retries with one stable delivery ID and eventually reports unhealthy.
    sqlx::query("UPDATE status_webhook_subscriptions SET endpoint=$2 WHERE id=$1")
        .bind(created.id)
        .bind("https://127.0.0.1/vox-status")
        .execute(db.pool())
        .await
        .unwrap();
    let worker = status.delivery_worker();
    let base = Utc::now();
    for attempt in 1..=8 {
        assert!(
            worker
                .deliver_next(
                    "status-test-worker",
                    base + chrono::Duration::minutes(attempt * 10)
                )
                .await
                .unwrap()
        );
    }
    assert!(
        !worker
            .deliver_next("status-test-worker", base + chrono::Duration::hours(2))
            .await
            .unwrap()
    );
    let (delivery_state, attempts): (String, i32) = sqlx::query_as(
        "SELECT state,attempts FROM status_webhook_deliveries
         WHERE subscription_id=$1 AND state='failed'",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!((delivery_state.as_str(), attempts), ("failed", 8));
    assert_eq!(
        status.list_subscriptions(&owner).await.unwrap()[0].state,
        "unhealthy"
    );

    status
        .disable_subscription(&owner, created.id)
        .await
        .unwrap();
    assert!(reopened.get(created.id).await.is_err());
    status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "waiting".into(),
                deduplication_key: format!("outbox-{}", Uuid::new_v4()),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    let after_disable: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM status_webhook_deliveries WHERE subscription_id=$1",
    )
    .bind(created.id)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(after_disable, 2);
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
