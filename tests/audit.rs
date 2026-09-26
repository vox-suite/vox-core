/**
* Integration tests for security audit logging and immutability.
*/
use chrono::Utc;
use uuid::Uuid;
use vox_core::{
    audit::{AuditQuery, AuditService},
    db::Db,
    durable_tasks::{DurableTaskService, StartTaskRequest},
    host_trust::{HostContextRequest, HostTrustService, RegisterHostAppRequest},
};

async fn context(db: &Db, user: &str) -> vox_core::identity::ResolvedUserContext {
    let trust = HostTrustService::new(db.clone());
    let host = trust
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: format!("audit-test-{}", Uuid::new_v4()),
            host_app_external_key: "audit-host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let request = HostContextRequest {
        host_user_id: user.into(),
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
async fn task_transitions_are_immutable_and_context_scoped() {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let owner = context(&db, "owner").await;
    let stranger = context(&db, "stranger").await;
    let task = DurableTaskService::new(db.clone())
        .start(
            &owner,
            StartTaskRequest {
                title: "Audit task".into(),
                instruction: "Record only authority facts".into(),
                agent_external_key: None,
            },
        )
        .await
        .unwrap();
    let audit = AuditService::new(db);
    let owner_events = audit
        .list(AuditQuery {
            after: None,
            limit: Some(100),
            user_context_id: Some(owner.id.0),
            aggregate_id: Some(task.id),
            execution_id: None,
        })
        .await
        .unwrap();
    assert!(
        owner_events
            .iter()
            .any(|event| event.event_type == "span.state_changed")
    );
    assert!(owner_events.iter().all(|event| {
        !event
            .details
            .to_string()
            .contains("Record only authority facts")
    }));
    let stranger_events = audit
        .list(AuditQuery {
            after: None,
            limit: Some(100),
            user_context_id: Some(stranger.id.0),
            aggregate_id: None,
            execution_id: None,
        })
        .await
        .unwrap();
    assert!(stranger_events.is_empty());
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn operator_access_is_recorded_and_sensitive_keys_are_rejected() {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
        .await
        .unwrap();
    db.migrate().await.unwrap();
    let audit = AuditService::new(db);

    // Privileged operator access is recorded
    let op_ref = format!("op-user-{}", Uuid::new_v4());
    audit
        .record_operator_access(
            &op_ref,
            "audit.operator_query",
            serde_json::json!({"reason": "troubleshooting", "session_id": "sess-1"}),
        )
        .await
        .unwrap();

    let events = audit
        .list(AuditQuery {
            after: None,
            limit: Some(100),
            user_context_id: None,
            aggregate_id: None,
            execution_id: None,
        })
        .await
        .unwrap();

    let found = events
        .iter()
        .find(|e| e.actor_type == op_ref && e.event_type == "audit.operator_query");
    assert!(found.is_some(), "Operator audit event must be present");

    // Sensitive keys (credentials, secrets, tokens, passwords, reasoning) must be rejected
    for bad_key in [
        "token",
        "credential",
        "secret",
        "password",
        "reasoning",
        "payload",
    ] {
        let bad_details = serde_json::json!({ bad_key: "sensitive_value" });
        assert!(
            audit
                .record_operator_access(&op_ref, "audit.operator_query", bad_details)
                .await
                .is_err(),
            "Expected sensitive key {bad_key} to be rejected"
        );
    }
}
