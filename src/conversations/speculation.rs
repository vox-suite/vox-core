/**
* Bounded voice metadata warmup and actor-scoped turn revision tracking.
*/
use super::{
    RespondRequest, SpeculateRequest,
    service::{ConversationError, ConversationService},
};
use crate::identity::ResourceOwner;
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct Key {
    owner: ResourceOwner,
    agent: String,
    channel: String,
    conversation: String,
    turn: String,
}
impl Key {
    fn responding(owner: ResourceOwner, request: &RespondRequest) -> Option<Self> {
        Some(Self {
            owner,
            agent: request.agent_external_key.clone(),
            channel: request.identity.channel.clone(),
            conversation: request.external_conversation_id.clone(),
            turn: request.turn_id.clone()?,
        })
    }
}
#[derive(Clone)]
struct Entry {
    revision: u64,
    text: String,
    started: Instant,
}

#[derive(Clone)]
pub(super) struct SpeculationCache(
    Arc<tokio::sync::Mutex<HashMap<Key, Entry>>>,
    Arc<tokio::sync::Semaphore>,
    Arc<tokio::sync::Mutex<HashMap<Key, (u64, Instant)>>>,
);
impl Default for SpeculationCache {
    fn default() -> Self {
        Self(
            Arc::default(),
            Arc::new(tokio::sync::Semaphore::new(32)),
            Arc::default(),
        )
    }
}
impl ConversationService {
    pub async fn speculate(
        &self,
        context: crate::identity::ResolvedUserContext,
        request: SpeculateRequest,
    ) -> Result<&'static str, ConversationError> {
        if request.text.trim().is_empty()
            || request.text.len() > 4096
            || request.turn_id.is_empty()
            || request.turn_id.len() > 128
            || request.external_conversation_id.is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        self.selected_agent(&context, &request.agent_external_key)
            .await?;
        let owner = context.owner();
        self.resolve_conversation(
            owner,
            &request.identity.channel,
            &request.external_conversation_id,
            &request.agent_external_key,
        )
        .await?;
        let key = Key {
            owner,
            agent: request.agent_external_key.clone(),
            channel: request.identity.channel.clone(),
            conversation: request.external_conversation_id.clone(),
            turn: request.turn_id.clone(),
        };
        if !self.register_revision(key.clone(), request.revision).await {
            return Ok("ignored");
        }
        let mut cache = self.speculative.0.lock().await;
        cache.retain(|_, entry| entry.started.elapsed() < Duration::from_secs(30));
        if let Some(entry) = cache.get_mut(&key) {
            if request.revision < entry.revision {
                return Ok("ignored");
            }
            if entry.text == request.text {
                entry.revision = request.revision;
                return Ok("deduplicated");
            }
        }
        if cache.len() >= 128 {
            return Ok("unavailable");
        }
        let Ok(permit) = self.speculative.1.clone().try_acquire_owned() else {
            return Ok("unavailable");
        };
        // Warm PostgreSQL's metadata indexes only. Discard results: no snapshot
        // is injected into model context, and actual library calls recheck access.
        let library = crate::agents::tools::library::AgentLibrary::new(
            Some(self.db.clone()),
            None,
            context,
            request.agent_external_key.clone(),
        );
        let query: String = request.text.chars().take(128).collect();
        tokio::spawn(async move {
            let _permit = permit;
            let _ = tokio::time::timeout(
                Duration::from_secs(5),
                library.invoke(crate::agents::tools::library::LibraryRequest::Search {
                    query,
                    offset: 0,
                }),
            )
            .await;
        });
        cache.insert(
            key,
            Entry {
                revision: request.revision,
                text: request.text,
                started: Instant::now(),
            },
        );
        Ok("started")
    }

    async fn register_revision(&self, key: Key, revision: u64) -> bool {
        let mut turns = self.speculative.2.lock().await;
        turns.retain(|_, (_, started)| started.elapsed() < Duration::from_secs(120));
        if turns
            .get(&key)
            .is_some_and(|(latest, _)| *latest > revision)
        {
            return false;
        }
        if turns.len() >= 1024 && !turns.contains_key(&key) {
            return false;
        }
        turns.insert(key, (revision, Instant::now()));
        true
    }

    pub(super) async fn register_final(
        &self,
        owner: ResourceOwner,
        request: &RespondRequest,
    ) -> bool {
        let (Some(_turn), Some(revision)) = (&request.turn_id, request.revision) else {
            return true;
        };
        let Some(key) = Key::responding(owner, request) else {
            return false;
        };
        self.register_revision(key, revision).await
    }

    pub(super) async fn is_current(&self, owner: ResourceOwner, request: &RespondRequest) -> bool {
        let (Some(_turn), Some(revision)) = (&request.turn_id, request.revision) else {
            return true;
        };
        let Some(key) = Key::responding(owner, request) else {
            return false;
        };
        self.speculative
            .2
            .lock()
            .await
            .get(&key)
            .is_some_and(|(latest, _)| *latest == revision)
    }

    pub(super) async fn persist_current_turn(
        &self,
        owner: ResourceOwner,
        request: &RespondRequest,
        conversation: super::ConversationId,
        text: &str,
    ) -> Result<(), ConversationError> {
        let turns = self.speculative.2.lock().await;
        if let Some(revision) = request.revision
            && let Some(key) = Key::responding(owner, request)
            && !turns
                .get(&key)
                .is_some_and(|(latest, _)| *latest == revision)
        {
            return Ok(());
        }
        self.append_turn(conversation, request.text.trim(), text)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_registry::{AgentMutation, AgentRegistry},
        agents::{
            AgentError,
            conversation::{ConversationPrompt, ConversationResponder},
        },
        db::Db,
        identity::IdentityService,
        memory::MemoryService,
    };
    use serde_json::json;
    use uuid::Uuid;

    struct ContextProbe;
    #[async_trait::async_trait]
    impl ConversationResponder for ContextProbe {
        async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
            Ok(prompt.user_context)
        }
    }

    fn request(agent: &str, conversation: &str, revision: u64) -> RespondRequest {
        serde_json::from_value(json!({
            "agent_external_key":agent,"identity":{"channel":"web","external_id":"fixture"},
            "external_conversation_id":conversation,"text":"Read my profile records calendar", "turn_id":"fixture-turn","revision":revision,
        })).unwrap()
    }
    fn warm(request: &RespondRequest) -> SpeculateRequest {
        SpeculateRequest {
            agent_external_key: request.agent_external_key.clone(),
            identity: request.identity.clone(),
            external_conversation_id: request.external_conversation_id.clone(),
            text: request.text.clone(),
            turn_id: request.turn_id.clone().unwrap(),
            revision: request.revision.unwrap(),
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL"]
    async fn voice_warmup_cannot_inject_user_wide_data_or_share_actor_revisions() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user:Uuid=sqlx::query_scalar("INSERT INTO users(profile_facts) VALUES('{\"private_global_canary\":\"never inject this\"}') RETURNING id").fetch_one(db.pool()).await.unwrap();
        let other: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let identities = IdentityService::new(db.clone());
        let a = identities.resolve_for_user(user).await.unwrap();
        let b = identities.resolve_for_user(other).await.unwrap();
        let registry = AgentRegistry::new(db.clone());
        let default = registry.owned_for_context(&a).await.unwrap()[0]
            .definition
            .external_key
            .clone();
        registry.owned_for_context(&b).await.unwrap();
        registry
            .mutate_owned(
                &a,
                AgentMutation::Create {
                    name: "Private specialist".into(),
                    instructions: "Keep this work private.".into(),
                },
            )
            .await
            .unwrap();
        let specialist = registry
            .owned_for_context(&a)
            .await
            .unwrap()
            .into_iter()
            .find(|v| !v.definition.is_default)
            .unwrap()
            .definition
            .external_key;
        let memory = MemoryService::new(db.clone(), None);
        memory
            .update_facts(
                a.owner(),
                &default,
                json!({"note":"default-only canary"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        memory
            .update_facts(
                a.owner(),
                &specialist,
                json!({"note":"specialist-only canary"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        let service = ConversationService::with_memory(db.clone(), Arc::new(ContextProbe), memory);
        let conversation = Uuid::new_v4().to_string();
        let ordinary = request(&default, &conversation, 1);
        assert_eq!(
            service.speculate(a.clone(), warm(&ordinary)).await.unwrap(),
            "started"
        );
        assert_eq!(
            service.speculate(a.clone(), warm(&ordinary)).await.unwrap(),
            "deduplicated"
        );
        // Wait for the bounded warmup worker, without depending on a timing sleep.
        let permits = tokio::time::timeout(
            Duration::from_secs(5),
            service.speculative.1.clone().acquire_many_owned(32),
        )
        .await
        .unwrap()
        .unwrap();
        drop(permits);
        let answer = service.respond(a.clone(), ordinary.clone()).await.unwrap();
        assert!(answer.text.contains("default-only canary"));
        assert!(!answer.text.contains("private_global_canary"));
        assert!(!answer.text.contains("specialist-only canary"));
        assert!(!answer.text.contains("lookup results"));
        let newer = request(&default, &conversation, 2);
        assert!(service.register_final(a.owner(), &newer).await);
        assert!(!service.is_current(a.owner(), &ordinary).await);
        assert!(matches!(
            service.respond(a.clone(), ordinary.clone()).await,
            Err(ConversationError::Invalid)
        ));
        // Even identical conversation/turn identifiers have independent actor revisions.
        let private = request(&specialist, &conversation, 1);
        assert!(service.register_final(a.owner(), &private).await);
        assert!(service.is_current(a.owner(), &private).await);
        assert!(!service.is_current(a.owner(), &ordinary).await);
        assert!(service.speculate(b.clone(), warm(&private)).await.is_err());
        let foreign = request(&default, &conversation, 1);
        assert!(service.register_final(b.owner(), &foreign).await);
        assert!(service.is_current(b.owner(), &foreign).await);
        assert!(!service.is_current(a.owner(), &ordinary).await);
        registry
            .mutate_owned(
                &a,
                AgentMutation::Archive {
                    agent_key: specialist.clone(),
                },
            )
            .await
            .unwrap();
        assert!(service.speculate(a, warm(&private)).await.is_err());
    }
}
