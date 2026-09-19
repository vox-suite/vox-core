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
    identity::UserId,
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
async fn test_full_voice_verification_and_identity_switch_flow() {
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
    assert_eq!(
        text2,
        "Your voice is not matching with Rahul. What is your name?"
    );

    // -------------------------------------------------------------------------
    // Step 3: Girlfriend introduces herself as "Priya"
    // System finds matching profile for Priya and asks for phone confirmation
    // -------------------------------------------------------------------------
    let req3 = Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"identity":{{"channel":"phone","external_id":"{rahul_phone}"}},"external_conversation_id":"CALL-2","text":"I'm Priya"}}"#
        )))
        .unwrap();

    let res3 = app.clone().oneshot(req3).await.unwrap();
    assert_eq!(res3.status(), StatusCode::OK);
    let b3 = to_bytes(res3.into_body(), 4096).await.unwrap();
    let text3 = serde_json::from_slice::<serde_json::Value>(&b3).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(
        text3,
        "I found a matching profile for Priya in my system. Can you tell me your phone number to confirm?"
    );

    // -------------------------------------------------------------------------
    // Step 4: Priya states her phone number to confirm identity
    // -------------------------------------------------------------------------
    let req4 = Request::post("/v1/conversations/respond")
        .header("authorization", "Bearer test-token")
        .header("content-type", "application/json")
        .body(Body::from(format!(
            r#"{{"identity":{{"channel":"phone","external_id":"{rahul_phone}"}},"external_conversation_id":"CALL-2","text":"It is 9123456789"}}"#
        )))
        .unwrap();

    let res4 = app.clone().oneshot(req4).await.unwrap();
    assert_eq!(res4.status(), StatusCode::OK);
    let b4 = to_bytes(res4.into_body(), 4096).await.unwrap();
    let text4 = serde_json::from_slice::<serde_json::Value>(&b4).unwrap()["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        text4.contains("Awesome, verified! Hello Priya!"),
        "got: {text4}"
    );

    // Verify database: Conversation user was updated to Priya's UUID!
    let conv_user: uuid::Uuid = sqlx::query_scalar(
        "SELECT user_id FROM conversations WHERE channel = 'phone' AND external_id = 'CALL-2'",
    )
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(conv_user, priya_uid);
}
