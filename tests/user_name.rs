use futures_util::StreamExt;
use rig::tool::Tool;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;
use vox_core::{
    agent_registry::AgentRegistry,
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder},
        tools::user_name::{UpdateUserName, UpdateUserNameArgs},
    },
    conversations::{RespondRequest, service::ConversationService},
    db::Db,
    identity::{ChannelIdentity, IdentityService, ResolvedUserContext},
    memory::{
        MemoryService,
        cache::{CacheError, ContextCache, MinimalUserInfo, RedisContextCache},
    },
};

struct Fixture {
    db: Db,
    memory: MemoryService,
    context: ResolvedUserContext,
    agent_key: String,
    conversation_id: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user: Uuid = sqlx::query_scalar(
            "INSERT INTO users(profile_facts) VALUES ('{\"keep\":\"existing fact\"}') RETURNING id",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        let context = IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        let agent_key = AgentRegistry::new(db.clone())
            .owned_for_context(&context)
            .await
            .unwrap()[0]
            .definition
            .external_key
            .clone();
        let conversation_id: Uuid = sqlx::query_scalar(
            "INSERT INTO conversations(user_id,user_context_id,agent_external_key,channel,external_conversation_id) VALUES($1,$2,$3,'web',$4) RETURNING id",
        ).bind(user).bind(context.id.0).bind(&agent_key).bind(Uuid::new_v4().to_string())
            .fetch_one(db.pool()).await.unwrap();
        let cache =
            Arc::new(RedisContextCache::new(&std::env::var("TEST_REDIS_URL").unwrap()).unwrap());
        let memory = MemoryService::new(db.clone(), Some(cache));
        Self {
            db,
            memory,
            context,
            agent_key,
            conversation_id,
        }
    }

    fn tool(&self, text: &str) -> UpdateUserName {
        UpdateUserName::new(
            Some(self.memory.clone()),
            self.context.owner(),
            self.agent_key.clone(),
            Some(self.conversation_id),
            text.into(),
        )
    }

    async fn profile(&self) -> (Option<String>, Value, i64) {
        sqlx::query_as("SELECT display_name,profile_facts,profile_version FROM users WHERE id=$1")
            .bind(self.context.user_id.0)
            .fetch_one(self.db.pool())
            .await
            .unwrap()
    }
}

fn args(name: &str, text: &str) -> UpdateUserNameArgs {
    UpdateUserNameArgs {
        name: name.into(),
        evidence_quote: text.into(),
    }
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL and TEST_REDIS_URL"]
async fn user_name_tool_saves_corrections_provenance_and_refreshes_redis() {
    let f = Fixture::new().await;
    for (name, text) in [
        ("Rahul Biswakarma", "My name is Rahul Biswakarma."),
        ("Jay", "Actually, my name is Jay."),
    ] {
        let result = f
            .tool(text)
            .call(&mut rig::prelude::ToolContext::new(), args(name, text))
            .await
            .unwrap();
        assert!(result.saved);
        assert!(result.cache_updated);
        assert_eq!(result.name, name);
        let (display_name, facts, _) = f.profile().await;
        assert_eq!(display_name.as_deref(), Some(name));
        assert_eq!(facts["name"], name);
        assert_eq!(facts["keep"], "existing fact");
        assert_eq!(
            facts["name_source"]["conversation_id"],
            f.conversation_id.to_string()
        );
        assert_eq!(facts["name_source"]["agent_key"], f.agent_key);
        assert_eq!(facts["name_source"]["evidence_quote"], text);
        assert!(facts["name_source"]["recorded_at"].is_string());
        let cached = f
            .memory
            .cache()
            .unwrap()
            .get_user(f.context.user_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(cached.name.as_deref(), Some(name));
        assert_eq!(
            cached.first_name.as_deref(),
            Some(name.split_whitespace().next().unwrap())
        );
    }
    assert_eq!(f.profile().await.2, 3);
    let private_memory_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM agent_memories WHERE user_context_id=$1")
            .bind(f.context.id.0)
            .fetch_one(f.db.pool())
            .await
            .unwrap();
    assert_eq!(private_memory_count, 0);
}

struct SmallTalkResponder;

#[async_trait::async_trait]
impl ConversationResponder for SmallTalkResponder {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        assert!(prompt.needs_onboarding);
        assert_eq!(prompt.user_name, None);
        Ok("Glad to hear that! How can I help?".into())
    }
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL and TEST_REDIS_URL"]
async fn small_talk_never_writes_a_name_in_sync_or_streaming_conversations() {
    for channel in ["web", "phone", "whatsapp"] {
        for streaming in [false, true] {
            let f = Fixture::new().await;
            let service = ConversationService::with_memory(
                f.db.clone(),
                Arc::new(SmallTalkResponder),
                f.memory.clone(),
            );
            let request = RespondRequest {
                agent_external_key: f.agent_key.clone(),
                identity: ChannelIdentity {
                    channel: channel.into(),
                    external_id: "fixture-user".into(),
                },
                external_conversation_id: Uuid::new_v4().to_string(),
                text: "I'm fine.".into(),
                initiation_context: Some("whatsapp_name:Unconfirmed Provider Name".into()),
                turn_id: None,
                revision: None,
                tts_provider: None,
                filler: None,
            };
            let reply = if streaming {
                let mut stream = service
                    .respond_stream(f.context.clone(), request)
                    .await
                    .unwrap();
                let mut text = String::new();
                while let Some(chunk) = stream.next().await {
                    text.push_str(&chunk.unwrap());
                }
                text
            } else {
                service
                    .respond(f.context.clone(), request)
                    .await
                    .unwrap()
                    .text
            };
            assert_eq!(reply, "Glad to hear that! How can I help?");
            let (display_name, facts, version) = f.profile().await;
            assert_eq!(display_name, None);
            assert_eq!(facts, json!({"keep":"existing fact"}));
            assert_eq!(version, 1);
            assert!(
                f.memory
                    .cache()
                    .unwrap()
                    .get_user(f.context.user_id)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL and TEST_REDIS_URL"]
async fn user_name_tool_rejects_foreign_conversations_and_disabled_users() {
    let victim = Fixture::new().await;
    let attacker = Fixture::new().await;
    let text = "My name is Rahul.";
    let tool = UpdateUserName::new(
        Some(attacker.memory.clone()),
        attacker.context.owner(),
        attacker.agent_key.clone(),
        Some(victim.conversation_id),
        text.into(),
    );
    assert!(
        tool.call(&mut rig::prelude::ToolContext::new(), args("Rahul", text))
            .await
            .is_err()
    );
    assert_eq!(victim.profile().await.0, None);
    assert_eq!(attacker.profile().await.0, None);
    sqlx::query("UPDATE users SET status='disabled' WHERE id=$1")
        .bind(attacker.context.user_id.0)
        .execute(attacker.db.pool())
        .await
        .unwrap();
    assert!(
        attacker
            .tool(text)
            .call(&mut rig::prelude::ToolContext::new(), args("Rahul", text))
            .await
            .is_err()
    );
    assert_eq!(attacker.profile().await.0, None);
}

struct UnavailableCache;

#[async_trait::async_trait]
impl ContextCache for UnavailableCache {
    async fn put_user(
        &self,
        _: vox_core::identity::UserId,
        _: &MinimalUserInfo,
    ) -> Result<(), CacheError> {
        Err(CacheError::Payload)
    }
}

#[tokio::test]
#[ignore = "requires isolated TEST_DATABASE_URL and TEST_REDIS_URL"]
async fn user_name_tool_reports_database_success_separately_from_cache_failure() {
    let f = Fixture::new().await;
    let memory = MemoryService::new(f.db.clone(), Some(Arc::new(UnavailableCache)));
    let text = "My name is Rahul.";
    let tool = UpdateUserName::new(
        Some(memory),
        f.context.owner(),
        f.agent_key.clone(),
        Some(f.conversation_id),
        text.into(),
    );
    let result = tool
        .call(&mut rig::prelude::ToolContext::new(), args("Rahul", text))
        .await
        .unwrap();
    assert!(result.saved);
    assert!(!result.cache_updated);
    assert_eq!(f.profile().await.0.as_deref(), Some("Rahul"));
}
