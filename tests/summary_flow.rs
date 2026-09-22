use async_trait::async_trait;
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};
use vox_core::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
        summarizer::{Summarizing, SummaryPrompt},
    },
    conversations::{CompleteConversationRequest, RespondRequest, service::ConversationService},
    db::Db,
    identity::{
        DeploymentId, HostAppId, IdentityService, ResourceOwner, UserContextSubject, UserId,
    },
    memory::{
        MemoryService,
        cache::{CacheError, ContextCache},
        projection,
    },
    summaries::{StructuredSummary, handler::SummaryHandler},
};

struct Responder;

#[async_trait]
impl ConversationResponder for Responder {
    async fn respond(&self, _: ConversationPrompt) -> Result<String, AgentError> {
        Ok("I will remember that".into())
    }
}

struct Summarizer;

#[async_trait]
impl Summarizing for Summarizer {
    async fn summarize(&self, _: SummaryPrompt) -> Result<StructuredSummary, AgentError> {
        Ok(StructuredSummary {
            recap: "User shared their preferred city".into(),
            profile_updates: BTreeMap::from([("preferred_city".into(), "Bengaluru".into())]),
            commitments: vec!["Send an update tomorrow".into()],
            decisions: vec!["Use phone notifications".into()],
        })
    }
}

struct FailingCache;

#[async_trait]
impl ContextCache for FailingCache {
    async fn get(&self, _: UserId) -> Result<Option<String>, CacheError> {
        Err(redis::RedisError::from((redis::ErrorKind::IoError, "offline")).into())
    }

    async fn set(&self, _: UserId, _: &str) -> Result<(), CacheError> {
        Err(redis::RedisError::from((redis::ErrorKind::IoError, "offline")).into())
    }
}

async fn setup() -> Db {
    let url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL is required");
    let db = Db::connect(&url).await.unwrap();
    db.migrate().await.unwrap();
    sqlx::query("TRUNCATE users CASCADE")
        .execute(db.pool())
        .await
        .unwrap();
    db
}

async fn owner(db: &Db, host_user_id: &str) -> ResourceOwner {
    let deployment_id: uuid::Uuid = sqlx::query_scalar("INSERT INTO platform_deployments (external_key) VALUES ('test.summary') ON CONFLICT (external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").fetch_one(db.pool()).await.unwrap();
    let host_app_id: uuid::Uuid = sqlx::query_scalar("INSERT INTO host_apps (deployment_id, external_key) VALUES ($1, 'test.host') ON CONFLICT (deployment_id, external_key) DO UPDATE SET external_key=EXCLUDED.external_key RETURNING id").bind(deployment_id).fetch_one(db.pool()).await.unwrap();
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn completion_and_summary_are_idempotent_when_redis_is_unavailable() {
    let db = setup().await;
    let service = ConversationService::new(db.clone(), Arc::new(Responder));
    let owner = owner(&db, "phone:+919999999997").await;
    let response = service
        .respond(
            owner,
            RespondRequest {
                channel: "phone".into(),
                external_conversation_id: "CA-summary".into(),
                text: "I prefer Bengaluru".into(),
                initiation_context: None,
                voice_signature: None,
                turn_id: None,
                revision: None,
                tts_provider: None,
            },
        )
        .await
        .unwrap();
    let completion = CompleteConversationRequest {
        channel: "phone".into(),
        external_conversation_id: "CA-summary".into(),
    };
    let (first, second) = tokio::join!(
        service.complete(owner, completion.clone()),
        service.complete(owner, completion)
    );
    first.unwrap();
    second.unwrap();
    let jobs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM jobs WHERE kind = 'summarize_conversation' AND payload_reference_id = $1",
    )
    .bind(response.conversation_id.0)
    .fetch_one(db.pool())
    .await
    .unwrap();
    assert_eq!(jobs, 1);

    let memory = MemoryService::new(db.clone(), Some(Arc::new(FailingCache)));
    let handler = SummaryHandler::with_memory(db.clone(), Arc::new(Summarizer), memory);
    handler.handle(response.conversation_id).await.unwrap();
    handler.handle(response.conversation_id).await.unwrap();
    let summaries: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_summaries")
        .fetch_one(db.pool())
        .await
        .unwrap();
    let facts: Value = sqlx::query_scalar("SELECT facts FROM user_profiles LIMIT 1")
        .fetch_one(db.pool())
        .await
        .unwrap();
    assert_eq!(summaries, 1);
    assert_eq!(facts["preferred_city"], "Bengaluru");
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn projection_drops_old_recaps_before_commitments_and_stays_valid_json() {
    let db = setup().await;
    let owner = owner(&db, "projection-user").await;
    let user_id = owner.user_id.0;
    for index in 0..20 {
        let conversation_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO conversations (user_context_id, user_id, channel, external_id, status) VALUES ($1, $2, 'phone', $3, 'completed') RETURNING id",
        )
        .bind(owner.user_context_id.0)
        .bind(user_id)
        .bind(format!("context-{index}"))
        .fetch_one(db.pool())
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO conversation_summaries (conversation_id, user_id, recap, commitments, decisions) VALUES ($1, $2, $3, $4, '[]')",
        )
        .bind(conversation_id)
        .bind(user_id)
        .bind("x".repeat(2_000))
        .bind(serde_json::json!(["Keep this commitment"]))
        .execute(db.pool())
        .await
        .unwrap();
    }
    let context = projection::build(&db, UserId(user_id)).await.unwrap();
    assert!(context.len() <= projection::MAX_CONTEXT_BYTES);
    let parsed: Value = serde_json::from_str(&context).unwrap();
    assert!(!parsed["commitments"].as_array().unwrap().is_empty());
    assert!(parsed["recent_recaps"].as_array().unwrap().len() < 20);
}
