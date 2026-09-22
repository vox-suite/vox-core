use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use chrono::Utc;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
    },
    db::Db,
    host_trust::{
        HostContextAssertion, HostContextRequest, HostTrustService, RegisterHostAppRequest,
    },
    http::{AppState, router},
};

struct GreetingAgent;

#[async_trait]
impl ConversationResponder for GreetingAgent {
    async fn respond(&self, _prompt: ConversationPrompt) -> Result<String, AgentError> {
        Ok("Hello Rahul".into())
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn authenticates_and_persists_a_conversation_turn() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(GreetingAgent),
        "test-token".into(),
    ));
    let host = HostTrustService::new(db.clone())
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: "test.conversations".into(),
            host_app_external_key: "test.host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let context = HostContextRequest {
        host_user_id: "phone:+919876543210".into(),
        organization_external_key: None,
    };
    let assertion = host
        .credential
        .sign_context_request(&context, Utc::now(), Uuid::new_v4())
        .unwrap();
    let payload = serde_json::json!({"host_context": context, "channel":"phone","external_conversation_id":"CA123","text":"Hello","initiation_context":null});
    let request = signed_request("/v1/conversations/respond", payload, &assertion);

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&body).unwrap()["text"],
        "Hello Rahul"
    );
    let roles: Vec<String> =
        sqlx::query_scalar("SELECT role FROM messages ORDER BY sequence_number")
            .fetch_all(db.pool())
            .await
            .unwrap();
    assert_eq!(roles, ["user", "assistant"]);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn schedules_and_events_require_fresh_signed_host_contexts() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(GreetingAgent),
        "test-token".into(),
    ));
    let host = HostTrustService::new(db.clone())
        .register_host_app(RegisterHostAppRequest {
            deployment_external_key: "test.authority-paths".into(),
            host_app_external_key: "test.host".into(),
            allowed_origins: vec![],
        })
        .await
        .unwrap();
    let context = HostContextRequest {
        host_user_id: "host-user-42".into(),
        organization_external_key: None,
    };

    let unsigned_event = app.clone().oneshot(Request::post("/v1/events")
        .header("authorization", "Bearer test-token").header("content-type", "application/json")
        .body(Body::from(r#"{"idempotency_key":"legacy-event","event_type":"test","occurred_at":"2026-09-22T00:00:00Z","payload":{}}"#)).unwrap()).await.unwrap();
    assert_eq!(unsigned_event.status(), StatusCode::UNAUTHORIZED);

    let event_assertion = host
        .credential
        .sign_context_request(&context, Utc::now(), Uuid::new_v4())
        .unwrap();
    let event = app.clone().oneshot(signed_request("/v1/events", serde_json::json!({
        "host_context": context.clone(), "idempotency_key":"signed-event", "event_type":"test",
        "occurred_at":"2026-09-22T00:00:00Z", "payload":{}
    }), &event_assertion)).await.unwrap();
    assert_eq!(event.status(), StatusCode::ACCEPTED);

    let schedule_assertion = host
        .credential
        .sign_context_request(&context, Utc::now(), Uuid::new_v4())
        .unwrap();
    let schedule = app
        .oneshot(signed_request(
            "/v1/schedules",
            serde_json::json!({
                "host_context": context, "instruction":"Send a reminder", "schedule_kind":"once",
                "run_at":"2026-09-23T00:00:00Z", "recurrence_expression":null, "timezone":"UTC"
            }),
            &schedule_assertion,
        ))
        .await
        .unwrap();
    assert_eq!(schedule.status(), StatusCode::CREATED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM events WHERE idempotency_key='legacy-event'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn legacy_service_token_and_channel_identity_are_rejected_before_work() {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(GreetingAgent),
        "test-token".into(),
    ));
    let response = app.oneshot(Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token").header("content-type", "application/json")
        .body(Body::from(r#"{"channel":"phone","identity":{"channel":"phone","external_id":"+919876543210"},"external_conversation_id":"legacy","text":"Hello"}"#)).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM conversations WHERE external_id='legacy'"
        )
        .fetch_one(db.pool())
        .await
        .unwrap(),
        0
    );
}

fn signed_request(
    uri: &str,
    payload: serde_json::Value,
    assertion: &HostContextAssertion,
) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .header(
            "x-vox-host-credential",
            assertion.credential_id().to_string(),
        )
        .header("x-vox-host-secret", assertion.secret())
        .header("x-vox-host-audience", assertion.audience())
        .header(
            "x-vox-host-timestamp",
            assertion.issued_at().timestamp().to_string(),
        )
        .header("x-vox-host-nonce", assertion.nonce().to_string())
        .header("x-vox-host-signature", assertion.signature())
        .body(Body::from(serde_json::to_vec(&payload).unwrap()))
        .unwrap()
}
