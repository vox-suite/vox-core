//! Manual public HTTPS webhook rehearsal against an isolated PostgreSQL database.
use chrono::Utc;
use uuid::Uuid;
use vox_core::{
    db::Db,
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
    status::{
        AppendStatusEvent, CreateSubscriptionRequest, EncryptedWebhookSecretStore, StatusService,
    },
};

async fn setup_context(db: &Db) -> vox_core::identity::ResolvedUserContext {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("status-live-{}", Uuid::new_v4()),
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

/// Manual public HTTPS rehearsal. The receiver records only synthetic UUIDs,
/// cursor values, and signature-verification results. Run against an isolated
/// database and a receiver that is controlled by this test's operator.
#[tokio::test]
#[ignore = "requires TEST_STATUS_LIVE_* and isolated PostgreSQL"]
async fn live_https_delivery_retry_rotation_and_recovery() {
    let endpoint = std::env::var("TEST_STATUS_LIVE_ENDPOINT").unwrap();
    let secret_file = std::env::var("TEST_STATUS_LIVE_SECRET_FILE").unwrap();
    let response_file = std::env::var("TEST_STATUS_LIVE_RESPONSE_FILE").unwrap();
    let receipts_file = std::env::var("TEST_STATUS_LIVE_RECEIPTS_FILE").unwrap();
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let owner = setup_context(&db).await;
    let key = "c3".repeat(32);
    let secrets =
        std::sync::Arc::new(EncryptedWebhookSecretStore::from_hex_key(db.clone(), &key).unwrap());
    let status = StatusService::new(db.clone()).with_secret_store(secrets);
    let created = status
        .create_subscription(&owner, CreateSubscriptionRequest { endpoint })
        .await
        .unwrap();
    std::fs::write(&secret_file, created.secret.unwrap()).unwrap();
    std::fs::write(&response_file, "503").unwrap();

    let first = status
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "queued".into(),
                deduplication_key: format!("live-first-{}", Uuid::new_v4()),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert!(
        status
            .delivery_worker()
            .deliver_next("live-first", Utc::now())
            .await
            .unwrap()
    );
    let (delivery_id, state): (Uuid, String) = sqlx::query_as(
        "SELECT id,state FROM status_webhook_deliveries WHERE subscription_id=$1 AND event_cursor=$2",
    )
    .bind(created.id)
    .bind(first.cursor)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(state, "queued");

    // A new service instance must recover the durable retry after the receiver returns 503.
    let reopened = StatusService::new(db.clone()).with_secret_store(std::sync::Arc::new(
        EncryptedWebhookSecretStore::from_hex_key(db.clone(), &key).unwrap(),
    ));
    std::fs::write(&response_file, "202").unwrap();
    assert!(
        reopened
            .delivery_worker()
            .deliver_next("live-restarted", Utc::now() + chrono::Duration::minutes(1))
            .await
            .unwrap()
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM status_webhook_deliveries WHERE id=$1")
            .bind(delivery_id)
            .fetch_one(db.pool())
            .await
            .unwrap();
    assert_eq!(state, "sent");
    let authoritative = reopened.list(&owner, 0, 200).await.unwrap();
    assert!(
        authoritative
            .iter()
            .any(|event| event.cursor == first.cursor)
    );

    let rotated = reopened
        .rotate_subscription(&owner, created.id)
        .await
        .unwrap();
    assert_eq!(rotated.secret_version, 2);
    std::fs::write(&secret_file, rotated.secret.unwrap()).unwrap();
    let second = reopened
        .append(
            &owner,
            AppendStatusEvent {
                aggregate_type: "task".into(),
                aggregate_id: Uuid::new_v4(),
                event_type: "task.state_changed".into(),
                state: "running".into(),
                deduplication_key: format!("live-second-{}", Uuid::new_v4()),
                occurred_at: Utc::now(),
            },
        )
        .await
        .unwrap();
    assert!(
        reopened
            .delivery_worker()
            .deliver_next("live-rotated", Utc::now() + chrono::Duration::minutes(2))
            .await
            .unwrap()
    );
    let second_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM status_webhook_deliveries WHERE subscription_id=$1 AND event_cursor=$2",
    )
    .bind(created.id)
    .bind(second.cursor)
    .fetch_one(db.pool())
    .await
    .unwrap();

    // Reconstruct an expired lease after the receiver accepted a hint but before
    // the worker could persist that response. The host must accept a duplicate ID.
    sqlx::query(
        "UPDATE status_webhook_deliveries SET state='sending',
         lease_until=now()-interval '1 second',lease_token=gen_random_uuid(),
         lease_owner='crashed' WHERE id=$1",
    )
    .bind(second_id)
    .execute(db.pool())
    .await
    .unwrap();
    assert!(
        reopened
            .delivery_worker()
            .deliver_next("live-recovered", Utc::now() + chrono::Duration::minutes(3))
            .await
            .unwrap()
    );
    reopened
        .disable_subscription(&owner, created.id)
        .await
        .unwrap();
    assert!(
        !reopened
            .delivery_worker()
            .deliver_next("live-disabled", Utc::now() + chrono::Duration::minutes(4))
            .await
            .unwrap()
    );

    let receipts: Vec<serde_json::Value> = std::fs::read_to_string(receipts_file)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(receipts.len(), 4);
    assert!(receipts.iter().all(|receipt| receipt["valid"] == true));
    assert_eq!(receipts[0]["response"], 503);
    assert_eq!(receipts[0]["delivery_id"], delivery_id.to_string());
    assert_eq!(receipts[1]["delivery_id"], delivery_id.to_string());
    assert_eq!(receipts[2]["delivery_id"], second_id.to_string());
    assert_eq!(receipts[3]["delivery_id"], second_id.to_string());
}
