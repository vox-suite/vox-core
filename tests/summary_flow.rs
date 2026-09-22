/**
* Integration tests for conversation summary generation flow.
*/
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
    identity::{ChannelIdentity, UserId},
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
    async fn get_user(
        &self,
        _: UserId,
    ) -> Result<Option<vox_core::memory::cache::MinimalUserInfo>, CacheError> {
        Err(redis::RedisError::from((redis::ErrorKind::IoError, "offline")).into())
    }

    async fn put_user(
        &self,
        _: UserId,
        _: &vox_core::memory::cache::MinimalUserInfo,
    ) -> Result<(), CacheError> {
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

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn completion_and_summary_are_idempotent_when_redis_is_unavailable() {
    let db = setup().await;
    let service = ConversationService::new(db.clone(), Arc::new(Responder));
    let identity = ChannelIdentity {
        channel: "phone".into(),
        external_id: "+919999999997".into(),
    };
    let response = service
        .respond(RespondRequest {
            identity: identity.clone(),
            external_conversation_id: "CA-summary".into(),
            text: "I prefer Bengaluru".into(),
            initiation_context: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
        })
        .await
        .unwrap();
    let completion = CompleteConversationRequest {
        identity,
        external_conversation_id: "CA-summary".into(),
    };
    let (first, second) = tokio::join!(
        service.complete(completion.clone()),
        service.complete(completion)
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
    let user_id: uuid::Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
        .fetch_one(db.pool())
        .await
        .unwrap();
    for index in 0..20 {
        let conversation_id: uuid::Uuid = sqlx::query_scalar(
            "INSERT INTO conversations (user_id, channel, external_id, status) VALUES ($1, 'phone', $2, 'completed') RETURNING id",
        )
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
