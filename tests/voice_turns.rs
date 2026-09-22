use async_trait::async_trait;
use futures_util::StreamExt;
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
    },
    conversations::{RespondRequest, SpeculateRequest, service::ConversationService},
    db::Db,
    identity::{DeploymentId, HostAppId, IdentityService, ResourceOwner, UserContextSubject},
    voiceprint::{VoiceSignature, VoiceprintService},
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

fn request(call: &str, text: &str) -> RespondRequest {
    RespondRequest {
        channel: "phone".into(),
        external_conversation_id: call.into(),
        text: text.into(),
        initiation_context: None,
        voice_signature: None,
        turn_id: Some("turn".into()),
        revision: Some(3),
        tts_provider: None,
    }
}

async fn owner(db: &Db, host_user_id: &str) -> ResourceOwner {
    let deployment_id: Uuid = sqlx::query_scalar("INSERT INTO platform_deployments (external_key) VALUES ('test.voice') ON CONFLICT (external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").fetch_one(db.pool()).await.unwrap();
    let host_app_id: Uuid = sqlx::query_scalar("INSERT INTO host_apps (deployment_id, external_key) VALUES ($1, 'test.host') ON CONFLICT (deployment_id, external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").bind(deployment_id).fetch_one(db.pool()).await.unwrap();
    IdentityService::new(db.clone())
        .resolve_context(&UserContextSubject {
            deployment_id: DeploymentId(deployment_id),
            host_app_id: HostAppId(host_app_id),
            organization_id: None,
            host_user_id: host_user_id.into(),
        })
        .await
        .unwrap()
        .owner()
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
async fn voice_mismatch_fails_closed_without_switching_owner() {
    let db = database().await;
    let caller_owner = owner(&db, "phone:caller").await;
    let speaker = owner(&db, "phone:candidate").await;
    sqlx::query("INSERT INTO user_profiles (user_id,facts) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET facts=$2").bind(caller_owner.user_id.0).bind(serde_json::json!({"name":"Owner"})).execute(db.pool()).await.unwrap();
    sqlx::query("INSERT INTO user_profiles (user_id,facts) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET facts=$2").bind(speaker.user_id.0).bind(serde_json::json!({"name":"Test"})).execute(db.pool()).await.unwrap();
    let service = ConversationService::new(db.clone(), Arc::new(Echo));
    let model = format!("onnx-sha256:{}", "a".repeat(64));
    let enrolled = VoiceSignature {
        features: vec![1.0, 0.0],
        sample_count: 1,
        sample_duration_ms: 1_500,
        model: Some(model.clone()),
    };
    VoiceprintService::new(db.clone())
        .save_voiceprint(caller_owner.user_id, &enrolled, 1)
        .await
        .unwrap();
    let call = Uuid::new_v4().to_string();
    let mut turn = request(&call, "What tasks are due?");
    turn.voice_signature = Some(
        VoiceSignature {
            features: vec![0.0, 1.0],
            sample_count: 1,
            sample_duration_ms: 1_500,
            model: Some(model),
        }
        .to_json(),
    );
    let denied = service.respond(caller_owner, turn).await.unwrap();
    assert!(denied.text.contains("couldn't verify"));
    let stored: Uuid = sqlx::query_scalar("SELECT user_id FROM conversations WHERE id=$1")
        .bind(denied.conversation_id.0)
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(stored, caller_owner.user_id.0);
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
    let owner = owner(&db, "phone:lookup").await;
    sqlx::query("INSERT INTO user_profiles (user_id,facts) VALUES ($1,$2) ON CONFLICT (user_id) DO UPDATE SET facts=$2")
        .bind(owner.user_id.0).bind(serde_json::json!({"name":"Tester","marker":"private-fact"})).execute(db.pool()).await.unwrap();
    let service = ConversationService::new(db.clone(), Arc::new(Echo)).with_jev(
        vox_core::jev::JevClient::new("test".into(), Some(format!("http://{address}/"))),
    );
    let call = Uuid::new_v4().to_string();
    let speculative = SpeculateRequest {
        channel: "phone".into(),
        external_conversation_id: call.clone(),
        text: "Read my profile".into(),
        turn_id: "turn".into(),
        revision: 1,
    };
    assert_eq!(
        service.speculate(owner, speculative.clone()).await.unwrap(),
        "started"
    );
    assert_eq!(
        service.speculate(owner, speculative).await.unwrap(),
        "deduplicated"
    );
    let mut stream = service
        .respond_stream(owner, request(&call, "Please read my profile"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(result.contains("Read-only lookup results"));
    let mut stream = service
        .respond_stream(owner, request(&call, "Delete all data"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(!result.contains("Read-only lookup results"));
    let mut stream = service
        .respond_stream(owner, request("different-call", "Read my profile"))
        .await
        .unwrap();
    let mut result = String::new();
    while let Some(chunk) = stream.next().await {
        result.push_str(&chunk.unwrap());
    }
    assert!(!result.contains("Read-only lookup results"));
    server.abort();
}
