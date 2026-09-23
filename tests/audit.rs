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
            .any(|event| event.event_type == "task.state_changed")
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
