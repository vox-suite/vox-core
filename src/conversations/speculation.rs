/**
* Actor-scoped turn revision tracking: a response for a superseded revision is dropped.
*/
use super::{
    RespondRequest,
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
#[derive(Clone, Default)]
pub(super) struct SpeculationCache(Arc<tokio::sync::Mutex<HashMap<Key, (u64, Instant)>>>);

impl ConversationService {
    async fn register_revision(&self, key: Key, revision: u64) -> bool {
        let mut turns = self.speculative.0.lock().await;
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
            .0
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
        let turns = self.speculative.0.lock().await;
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
