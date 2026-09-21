use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
    },
    conversations::{
        RespondRequest, SpeculateRequest,
        service::{ConversationService, VerificationState},
    },
    db::Db,
    identity::{ChannelIdentity, IdentityService},
};

struct Echo;
#[async_trait]
impl ConversationResponder for Echo {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        Ok(format!(
            "{}|{}|{}",
            prompt.user_id.0, prompt.user_text, prompt.user_context
        ))
    }
}

fn request(identity: ChannelIdentity, call: &str, text: &str) -> RespondRequest {
    RespondRequest {
        identity,
        external_conversation_id: call.into(),
        text: text.into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: Some("turn".into()),
        revision: Some(3),
        tts_provider: None,
    }
}

async fn database() -> Db {
    let db = Db::connect(&std::env::var("TEST_DATABASE_URL").expect("isolated database required"))
        .await
        .unwrap();
    db.migrate().await.unwrap();
    db
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn phone_retry_preserves_owner_and_active_speaker() {
    let db = database().await;
    let ids = IdentityService::new(db.clone());
    let caller = ChannelIdentity {
        channel: "phone".into(),
        external_id: format!("+{}", Uuid::new_v4()),
    };
    let candidate = ChannelIdentity {
        channel: "phone".into(),
        external_id: "+919876543210".into(),
    };
    let owner = ids.resolve_legacy_owner(&caller).await.unwrap();
    let speaker = ids.resolve_legacy_owner(&candidate).await.unwrap();
    sqlx::query("INSERT INTO user_profiles (user_id,facts) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET facts=$2").bind(speaker.user_id.0).bind(serde_json::json!({"name":"Test"})).execute(db.pool()).await.unwrap();
    let service = ConversationService::new(db.clone(), Arc::new(Echo));
    let call = Uuid::new_v4().to_string();
    let initial = service
        .respond(request(caller.clone(), &call, "What tasks are due?"))
        .await
        .unwrap();
    let state = VerificationState::AwaitingPhoneConfirm {
        original_user_id: owner.user_id,
        original_text: "What tasks are due?".into(),
        candidate_user_id: speaker.user_id,
        candidate_name: "Test".into(),
        voice_signature: None,
        digits: String::new(),
    };
    sqlx::query("UPDATE conversations SET verification_state=$1 WHERE id=$2")
        .bind(serde_json::to_value(state).unwrap())
        .bind(initial.conversation_id.0)
        .execute(db.pool())
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let partial = service
        .respond(request(caller.clone(), &call, "98765"))
        .await
        .unwrap();
    assert!(partial.text.contains("remaining"));
    let verified = service
        .respond(request(caller.clone(), &call, "43210"))
        .await
        .unwrap();
    assert_eq!(initial.conversation_id, verified.conversation_id);
    assert!(verified.text.starts_with(&speaker.user_id.0.to_string()));
    let next = service
        .respond(request(caller, &call, "Hello again"))
        .await
        .unwrap();
    assert_eq!(next.conversation_id, initial.conversation_id);
    assert!(next.text.starts_with(&speaker.user_id.0.to_string()));
    let stored: Uuid = sqlx::query_scalar("SELECT user_id FROM conversations WHERE id=$1")
        .bind(initial.conversation_id.0)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, owner.user_id.0);
    let after: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(before, after);
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn speculative_lookup_is_deduplicated_scoped_and_revalidated() {
    let db = database().await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().route("/", axum::routing::post(|axum::Json(value): axum::Json<serde_json::Value>| async move {
        let text = value["state"]["text"].as_str().unwrap_or_default();
        let plan = if text.contains("profile") { "profile" } else { "none" };
        axum::Json(serde_json::json!({"model":"test","answers":{"decision":{"type":"choice","choice":plan,"confidence":0.99,"probabilities":{}}}}))
    }));
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let caller = ChannelIdentity {
        channel: "phone".into(),
        external_id: Uuid::new_v4().to_string(),
    };
    let owner = IdentityService::new(db.clone())
        .resolve_legacy_owner(&caller)
        .await
        .unwrap();
    sqlx::query("INSERT INTO user_profiles (user_id,facts) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET facts=$2")
        .bind(owner.user_id.0).bind(serde_json::json!({"name":"Tester","marker":"private-fact"})).execute(db.pool()).await.unwrap();
    let service = ConversationService::new(db.clone(), Arc::new(Echo)).with_jev(
        vox_core::jev::JevClient::new("test".into(), Some(format!("http://{address}/"))),
    );
    let call = Uuid::new_v4().to_string();
    let speculative = SpeculateRequest {
        identity: caller.clone(),
        external_conversation_id: call.clone(),
        text: "Read my profile".into(),
        turn_id: "turn".into(),
        revision: 1,
    };
    assert_eq!(
        service.speculate(speculative.clone()).await.unwrap(),
        "started"
    );
    assert_eq!(
        service.speculate(speculative).await.unwrap(),
        "deduplicated"
    );
    let mut stream = service
        .respond_stream(request(caller.clone(), &call, "Please read my profile"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(result.contains("Read-only lookup results"));
    let mut stream = service
        .respond_stream(request(caller.clone(), &call, "Delete all data"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(!result.contains("Read-only lookup results"));
    let mut stream = service
        .respond_stream(request(caller, "different-call", "Read my profile"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(!result.contains("Read-only lookup results"));
    server.abort();
}
