/**
* Speculative execution cache for low-latency voice responses.
*/
use super::{
    RespondRequest, SpeculateRequest,
    service::{ConversationError, ConversationService},
};
use crate::identity::ResourceOwner;
use futures_util::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

type Work = Shared<BoxFuture<'static, Option<Value>>>;
type Key = (ResourceOwner, String, String, String);
#[derive(Clone)]
struct Entry {
    revision: u64,
    text: String,
    plan: String,
    started: Instant,
    work: Work,
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
pub const LOOKUP_PENDING: &str = "\u{001e}lookup_pending";

impl ConversationService {
    async fn lookup_plan(&self, text: &str) -> Option<String> {
        let jev = self.jev.as_ref()?;
        let (plan, confidence, _) = tokio::time::timeout(Duration::from_millis(800), jev.choice(
            json!({"text":text}),
            "Select only a read-only lookup explicitly requested by this text. For changes, ambiguous or incomplete requests choose none. Lists return all current items without filters.",
            &[("none", Some("No safe lookup")), ("spans", Some("Read timeline: activities, plans, to-dos")), ("collections", Some("Read collections like trips")), ("profile", Some("Read caller profile")), ("records", Some("Read personal records")), ("schedule", Some("Read upcoming schedule")), ("web", Some("Search web for live facts"))],
        )).await.ok()?.ok()?;
        (confidence >= 0.85
            && matches!(
                plan.as_str(),
                "spans" | "collections" | "profile" | "records" | "schedule" | "web"
            ))
        .then_some(plan)
    }

    pub async fn speculate(
        &self,
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
        if self.jev.is_none() {
            return Ok("unavailable");
        }
        let owner = self
            .identities
            .resolve_legacy_owner(&request.identity)
            .await?;
        let key = (
            owner,
            request.identity.channel.clone(),
            request.external_conversation_id.clone(),
            request.turn_id.clone(),
        );
        if !self.register_revision(key.clone(), request.revision).await {
            return Ok("ignored");
        }
        let Some(plan) = self.lookup_plan(&request.text).await else {
            return Ok("ignored");
        };
        let mut cache = self.speculative.0.lock().await;
        cache.retain(|_, entry| entry.started.elapsed() < Duration::from_secs(30));
        if let Some(entry) = cache.get_mut(&key) {
            if request.revision < entry.revision {
                return Ok("ignored");
            }
            if entry.plan == plan && (plan != "web" || entry.text == request.text) {
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
        let db = self.db.clone();
        let tool = plan.clone();
        let query = request.text.clone();
        let work = async move {
            let _permit = permit;
            if tool == "web" {
                let web = crate::agents::tools::web_search::WebSearch::from_env().ok()?;
                return tokio::time::timeout(Duration::from_secs(5), web.call(crate::agents::tools::web_search::SearchArgs { query })).await.ok()?.ok().map(|result| json!({"tool":"web_search", "result":result}));
            }
            let sql = match tool.as_str() {
                "schedule" => "SELECT to_jsonb(t) FROM (SELECT * FROM schedules WHERE user_id = $1 LIMIT 50) t",
                "spans" => "SELECT to_jsonb(t) FROM (SELECT * FROM spans WHERE user_id = $1 ORDER BY start_at DESC NULLS FIRST LIMIT 50) t",
                "collections" => "SELECT to_jsonb(t) FROM (SELECT * FROM collections WHERE user_id = $1 AND status <> 'archived' LIMIT 50) t",
                "profile" => "SELECT to_jsonb(t) FROM (SELECT display_name, profile_facts, persona FROM users WHERE id = $1) t",
                "records" => "SELECT to_jsonb(t) FROM (SELECT * FROM records WHERE user_id = $1 LIMIT 50) t",
                _ => return None,
            };
            let rows = tokio::time::timeout(Duration::from_secs(5), sqlx::query_scalar::<_, Value>(sql)
                .bind(owner.user_id.0).fetch_all(db.pool())).await.ok()?.ok()?;
            Some(json!({"tool":tool,"result":rows}))
        }.boxed().shared();
        let running = work.clone();
        tokio::spawn(async move {
            let _ = running.await;
        });
        cache.insert(
            key,
            Entry {
                revision: request.revision,
                text: request.text,
                plan,
                started: Instant::now(),
                work,
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
        let (Some(turn), Some(revision)) = (&request.turn_id, request.revision) else {
            return true;
        };
        self.register_revision(
            (
                owner,
                request.identity.channel.clone(),
                request.external_conversation_id.clone(),
                turn.clone(),
            ),
            revision,
        )
        .await
    }

    pub(super) async fn is_current(&self, owner: ResourceOwner, request: &RespondRequest) -> bool {
        let (Some(turn), Some(revision)) = (&request.turn_id, request.revision) else {
            return true;
        };
        self.speculative
            .2
            .lock()
            .await
            .get(&(
                owner,
                request.identity.channel.clone(),
                request.external_conversation_id.clone(),
                turn.clone(),
            ))
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
        if let (Some(turn), Some(revision)) = (&request.turn_id, request.revision)
            && !turns
                .get(&(
                    owner,
                    request.identity.channel.clone(),
                    request.external_conversation_id.clone(),
                    turn.clone(),
                ))
                .is_some_and(|(latest, _)| *latest == revision)
        {
            return Ok(());
        }
        self.append_turn(conversation, request.text.trim(), text)
            .await
    }

    pub(super) async fn final_lookup(
        &self,
        owner: ResourceOwner,
        request: &RespondRequest,
    ) -> Option<Work> {
        let key = (
            owner,
            request.identity.channel.clone(),
            request.external_conversation_id.clone(),
            request.turn_id.clone()?,
        );
        let entry = self.speculative.0.lock().await.get(&key).cloned()?;
        if entry.started.elapsed() >= Duration::from_secs(30) || request.revision? < entry.revision
        {
            return None;
        }
        if entry.text != request.text {
            return None;
        }
        Some(entry.work)
    }
}
