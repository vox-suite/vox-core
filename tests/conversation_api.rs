/**
* Integration tests for conversational HTTP API endpoints.
*/
use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use std::sync::Arc;
use tower::ServiceExt;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
    },
    db::Db,
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
    let request = Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(r#"{"identity":{"channel":"phone","external_id":"+919876543210"},"external_conversation_id":"CA123","text":"Hello","initiation_context":null}"#)).unwrap();

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
