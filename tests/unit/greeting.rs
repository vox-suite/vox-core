/**
* Database greeting behavior and transaction isolation tests.
*/
use super::Db;
use crate::{
agents::{
    AgentError,
    conversation::{ConversationPrompt, ConversationResponder},
},
conversations::{RespondRequest, service::ConversationService},
identity::{ChannelIdentity, UserId},
memory::{
    MemoryService,
    cache::{CacheError, ContextCache},
},
};
use async_trait::async_trait;
use futures_util::StreamExt;
use sqlx::postgres::PgPoolOptions;
use std::{sync::Arc, time::Duration};

struct UnusedAgent;

#[async_trait]
impl ConversationResponder for UnusedAgent {
async fn respond(&self, _: ConversationPrompt) -> Result<String, AgentError> {
    panic!("opening must not invoke the agent")
}
}

struct GreetingCache {
name: Option<String>,
delay: Duration,
}

#[async_trait]
impl ContextCache for GreetingCache {
async fn get(&self, _: UserId) -> Result<Option<String>, CacheError> {
    panic!("opening must not load user context")
}

async fn set(&self, _: UserId, _: &str) -> Result<(), CacheError> {
    Ok(())
}

async fn get_greeting_name(
    &self,
    channel: &str,
    external_id: &str,
) -> Result<Option<String>, CacheError> {
    assert_eq!(channel, "phone");
    assert_eq!(external_id, "+919876543210");
    tokio::time::sleep(self.delay).await;
    Ok(self.name.clone())
}
}

async fn greeting(name: Option<&str>, delay: Duration) -> String {
let db = Db {
    pool: PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy("postgres://unused:unused@127.0.0.1:1/unused")
        .unwrap(),
};
let memory = MemoryService::new(
    db.clone(),
    Some(Arc::new(GreetingCache {
        name: name.map(str::to_owned),
        delay,
    })),
);
let service = ConversationService::with_memory(db, Arc::new(UnusedAgent), memory);
let request = RespondRequest {
    identity: ChannelIdentity {
        channel: "phone".into(),
        external_id: "+919876543210".into(),
    },
    external_conversation_id: "CA-cache-greeting".into(),
    text: "The call just connected. Greet the user.".into(),
    initiation_context: None,
    turn_id: None,
    revision: None,
    tts_provider: None,
};
tokio::time::timeout(Duration::from_millis(500), async {
    let mut stream = service.respond_stream(request).await.unwrap();
    let mut text = String::new();
    while let Some(chunk) = stream.next().await {
        text.push_str(&chunk.unwrap());
    }
    text
})
.await
.expect("greeting must finish while PostgreSQL is unavailable")
}

#[tokio::test]
async fn cached_greeting_does_not_wait_for_database() {
assert_eq!(
    greeting(Some("Rahul"), Duration::ZERO).await,
    "Hello Rahul! How can I help you today?"
);
}

#[tokio::test]
async fn cache_miss_greets_without_database() {
assert_eq!(
    greeting(None, Duration::ZERO).await,
    "Hi there! It seems you're calling for the first time. How can I help you?"
);
}

#[tokio::test]
async fn slow_cache_falls_back_without_database() {
assert_eq!(
    greeting(Some("Rahul"), Duration::from_secs(30)).await,
    "Hi there! It seems you're calling for the first time. How can I help you?"
);
}

#[tokio::test]
async fn replies_and_hangup_wait_for_opening_even_after_stream_is_dropped() {
let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
let address = listener.local_addr().unwrap();
let db = Db {
    pool: PgPoolOptions::new()
        .acquire_timeout(Duration::from_secs(3))
        .connect_lazy(&format!(
            "postgres://unused:unused@{address}/unused?sslmode=disable"
        ))
        .unwrap(),
};
let memory = MemoryService::new(
    db.clone(),
    Some(Arc::new(GreetingCache {
        name: Some("Rahul".into()),
        delay: Duration::ZERO,
    })),
);
let service = ConversationService::with_memory(db, Arc::new(UnusedAgent), memory);
let mut request = RespondRequest {
    identity: ChannelIdentity {
        channel: "phone".into(),
        external_id: "+919876543210".into(),
    },
    external_conversation_id: "CA-background-opening".into(),
    text: "The call just connected. Greet the user.".into(),
    initiation_context: None,
    turn_id: None,
    revision: None,
    tts_provider: None,
};
let greeting = service.respond_stream(request.clone()).await.unwrap();
drop(greeting);
let (_opening_connection, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
    .await
    .expect("initialization must outlive the greeting stream")
    .unwrap();
request.text = "What's on my calendar?".into();
let reply_service = service.clone();
let reply_request = request.clone();
let reply = tokio::spawn(async move { reply_service.respond_stream(reply_request).await });
let unary_service = service.clone();
let unary_request = request.clone();
let unary = tokio::spawn(async move { unary_service.respond(unary_request).await });
let hangup = tokio::spawn(async move {
    service
        .complete(crate::conversations::CompleteConversationRequest {
            identity: request.identity,
            external_conversation_id: request.external_conversation_id,
        })
        .await
});
assert!(
    tokio::time::timeout(Duration::from_millis(150), listener.accept())
        .await
        .is_err(),
    "reply and hangup must not access PostgreSQL before initialization finishes"
);
assert!(!reply.is_finished());
assert!(!unary.is_finished());
assert!(!hangup.is_finished());
reply.abort();
unary.abort();
hangup.abort();
}
