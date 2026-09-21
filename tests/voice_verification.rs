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
    voiceprint::VoiceSignature,
};

struct DummyAgent;

#[async_trait]
impl ConversationResponder for DummyAgent {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        Ok(format!("Agent response for user {:?}", prompt.user_id))
    }
}

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.expect("connect database");
    db.migrate().await.expect("migrate database");
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    db
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn synthetic_voice_cannot_override_phone_identity() {
    let db = setup().await;
    let app = router(AppState::with_dependencies(
        db.clone(),
        Arc::new(DummyAgent),
        "test-token".into(),
    ));

    let rahul_phone = "+919876543210";
    let priya_phone = "+919123456789";

    // 0. Pre-seed an existing user "Priya" with phone "+919123456789"
    let priya_uid: uuid::Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
         VALUES ($1, '{\"name\":\"Priya\"}'::jsonb, 1, now())",
    )
    .bind(priya_uid)
    .execute(db.pool())
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, 'phone', $2)",
    )
    .bind(priya_uid)
    .bind(priya_phone)
    .execute(db.pool())
    .await
    .unwrap();

    let sig_rahul = VoiceSignature::new(vec![1.0, 0.0, 0.0]);
    let sig_different = VoiceSignature::new(vec![0.0, 1.0, 0.0]); // orthogonal (similarity = 0.0)

    // -------------------------------------------------------------------------
    // Step 1: First call from Rahul's number - onboard name + enroll voiceprint
    // -------------------------------------------------------------------------
    let req1 = Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"identity":{{"channel":"phone","external_id":"{rahul_phone}"}},"external_conversation_id":"CALL-1","text":"My name is Rahul","voice_signature":"{}"}}"#,
            sig_rahul.to_json().replace('"', "\\\"")
        )))
        .unwrap();

    let res1 = app.clone().oneshot(req1).await.unwrap();
    assert_eq!(res1.status(), StatusCode::OK);
    let b1 = to_bytes(res1.into_body(), 4096).await.unwrap();
    let text1 = serde_json::from_slice::<serde_json::Value>(&b1).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text1.contains("Nice to meet you Rahul!"), "got: {text1}");

    // -------------------------------------------------------------------------
    // Step 2: Next call from Rahul's phone - Girlfriend speaks with different voice
    // -------------------------------------------------------------------------
    let req2 = Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"identity":{{"channel":"phone","external_id":"{rahul_phone}"}},"external_conversation_id":"CALL-2","text":"Hey, is my order ready?","voice_signature":"{}"}}"#,
            sig_different.to_json().replace('"', "\\\"")
        )))
        .unwrap();

    let res2 = app.clone().oneshot(req2).await.unwrap();
    assert_eq!(res2.status(), StatusCode::OK);
    let b2 = to_bytes(res2.into_body(), 4096).await.unwrap();
    let text2 = serde_json::from_slice::<serde_json::Value>(&b2).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(text2.starts_with("Agent response for user"));
    let enrolled: i64 = sqlx::query_scalar("SELECT count(*) FROM user_voiceprints")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(enrolled, 0);
    let conv_user: uuid::Uuid = sqlx::query_scalar(
        "SELECT user_id FROM conversations WHERE channel = 'phone' AND external_id = 'CALL-2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_ne!(conv_user, priya_uid);
}
