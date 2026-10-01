/**
 * Context cache, memory projection, and user name resolution.
 */
pub mod cache;
pub mod greetings;
pub mod projection;

use crate::{
    db::Db,
    identity::{ResourceOwner, UserId},
};
use cache::{MinimalChannel, MinimalUserInfo, device_kinds};
use std::sync::Arc;

#[derive(Clone)]
pub struct MemoryService {
    db: Db,
    cache: Option<Arc<dyn cache::ContextCache>>,
}

impl MemoryService {
    pub fn new(db: Db, cache: Option<Arc<dyn cache::ContextCache>>) -> Self {
        Self { db, cache }
    }

    pub fn cache(&self) -> Option<&Arc<dyn cache::ContextCache>> {
        self.cache.as_ref()
    }

    /// Agent-owned model context. Read current ownership and archive state on every load.
    /// Identity caches below contain only channel/name routing, never this projection.
    pub async fn load(&self, owner: ResourceOwner, agent_key: &str) -> Result<String, sqlx::Error> {
        projection::build(&self.db, owner, agent_key).await
    }

    pub async fn get_user_name(&self, user_id: UserId) -> Result<Option<String>, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(info)) = cache.get_user(user_id).await
            && let Some(name) = info
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
        {
            return Ok(Some(name.to_owned()));
        }

        let name: Option<String> =
            sqlx::query_scalar(
                "SELECT COALESCE(NULLIF(profile_facts->>'name', ''), display_name) FROM users WHERE id = $1",
            )
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await?
                .flatten();

        if let Some(ref n) = name {
            let _ = self.write_minimal_user(user_id, Some(n)).await;
        }

        Ok(name)
    }

    pub async fn set_user_name(&self, user_id: UserId, name: &str) -> Result<(), sqlx::Error> {
        let trimmed = name.trim();
        sqlx::query(
            "UPDATE users SET \
             profile_facts = jsonb_set(profile_facts, '{name}', to_jsonb($2::text), true), \
             display_name = $2, \
             profile_version = profile_version + 1, \
             updated_at = now() \
             WHERE id = $1",
        )
        .bind(user_id.0)
        .bind(trimmed)
        .execute(self.db.pool())
        .await?;

        let _ = self.write_minimal_user(user_id, Some(trimmed)).await;
        Ok(())
    }

    pub async fn find_user_by_name(&self, name: &str) -> Result<Option<UserId>, sqlx::Error> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let user_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT id FROM users \
             WHERE LOWER(COALESCE(NULLIF(profile_facts->>'name', ''), display_name, '')) = LOWER($1) \
             LIMIT 1",
        )
        .bind(trimmed)
        .fetch_optional(self.db.pool())
        .await?;

        Ok(user_id.map(UserId))
    }

    /// Re-derive the minimal Redis record (name + channel index) for a user
    /// from Postgres. Call this after any write that reassigns a
    /// `channel_identities` row (e.g. merging accounts on phone link) so the
    /// `vox:channel:*` index doesn't keep pointing at the previous owner
    /// until the next hourly reconciliation sweep.
    pub async fn refresh_minimal_user(&self, user_id: UserId) -> Result<(), sqlx::Error> {
        self.write_minimal_user(user_id, None).await
    }

    async fn write_minimal_user(
        &self,
        user_id: UserId,
        name_override: Option<&str>,
    ) -> Result<(), sqlx::Error> {
        let Some(cache) = &self.cache else {
            return Ok(());
        };
        let name = if let Some(name) = name_override {
            Some(name.to_owned())
        } else {
            sqlx::query_scalar(
                "SELECT COALESCE(NULLIF(profile_facts->>'name', ''), display_name) FROM users WHERE id = $1",
            )
            .bind(user_id.0)
            .fetch_optional(self.db.pool())
            .await?
            .flatten()
        };
        let channels = sqlx::query_as::<_, (String, String)>(
            "SELECT channel, normalized_external_id FROM channel_identities \
             WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .fetch_all(self.db.pool())
        .await?
        .into_iter()
        .map(|(channel, external_id)| MinimalChannel {
            channel,
            external_id,
        })
        .collect();
        let platforms: Vec<String> = sqlx::query_scalar(
            "SELECT platform FROM devices WHERE user_id = $1 AND revoked_at IS NULL",
        )
        .bind(user_id.0)
        .fetch_all(self.db.pool())
        .await?;
        let has_mobile_consent: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM data_source_consents \
             WHERE user_id = $1 AND granted_at IS NOT NULL AND revoked_at IS NULL)",
        )
        .bind(user_id.0)
        .fetch_one(self.db.pool())
        .await?;
        let devices = device_kinds(platforms.iter().map(String::as_str), has_mobile_consent);
        let info = MinimalUserInfo::new(name, channels, devices);
        let _ = cache.put_user(user_id, &info).await;
        Ok(())
    }
}

/// Hold current actor rows through a memory read/write so archive cannot race mutation.
async fn active_actor(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    owner: ResourceOwner,
    agent_key: &str,
) -> Result<uuid::Uuid, sqlx::Error> {
    // Follow the same context → agent lock order used by owned-agent mutations.
    sqlx::query_scalar::<_, uuid::Uuid>(
        "SELECT id FROM user_contexts WHERE id=$1 AND user_id=$2 FOR SHARE",
    )
    .bind(owner.user_context_id.0)
    .bind(owner.user_id.0)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(sqlx::Error::RowNotFound)?;
    sqlx::query_scalar("SELECT a.id FROM agent_definitions a JOIN user_contexts u ON u.id=a.owner_user_context_id AND u.deployment_id=a.deployment_id JOIN agent_definitions t ON t.id=a.template_id JOIN deployment_agent_selections s ON s.agent_definition_id=a.id AND s.deployment_id=a.deployment_id WHERE a.owner_user_context_id=$1 AND u.user_id=$2 AND a.external_key=$3 AND a.state='enabled' AND t.state='enabled' FOR SHARE OF a,t,s")
        .bind(owner.user_context_id.0).bind(owner.user_id.0).bind(agent_key)
        .fetch_optional(&mut **tx).await?.ok_or(sqlx::Error::RowNotFound)
}

impl MemoryService {
    pub async fn update_facts(
        &self,
        owner: ResourceOwner,
        agent_key: &str,
        facts: serde_json::Map<String, serde_json::Value>,
    ) -> Result<serde_json::Value, sqlx::Error> {
        let delta = serde_json::Value::Object(facts);
        if serde_json::to_vec(&delta)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?
            .len()
            > 8192
        {
            return Err(sqlx::Error::Protocol("agent facts exceed 8 KiB".into()));
        }
        let mut tx = self.db.pool().begin().await?;
        let actor = active_actor(&mut tx, owner, agent_key).await?;
        let saved = sqlx::query_scalar("INSERT INTO agent_memories(user_context_id,agent_id,facts) VALUES($1,$2,$3) ON CONFLICT(user_context_id,agent_id) DO UPDATE SET facts=agent_memories.facts || EXCLUDED.facts,version=agent_memories.version+1,updated_at=now() WHERE agent_memories.retention_enabled RETURNING facts")
            .bind(owner.user_context_id.0).bind(actor).bind(delta).fetch_optional(&mut *tx).await?.ok_or(sqlx::Error::RowNotFound)?;
        tx.commit().await?;
        Ok(saved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent_registry::{AgentMutation, AgentRegistry},
        identity::IdentityService,
    };
    use serde_json::json;
    use uuid::Uuid;

    struct SummaryFixture;
    #[async_trait::async_trait]
    impl crate::agents::summarizer::Summarizing for SummaryFixture {
        async fn summarize(
            &self,
            _: crate::agents::summarizer::SummaryPrompt,
        ) -> Result<crate::summaries::StructuredSummary, crate::agents::AgentError> {
            Ok(crate::summaries::StructuredSummary {
                recap: "specialist-only summary".into(),
                profile_updates: std::collections::BTreeMap::from([(
                    "private_detail".into(),
                    "specialist-only fact".into(),
                )]),
                commitments: vec!["specialist-only commitment".into()],
                decisions: vec![],
            })
        }
    }

    #[tokio::test]
    #[ignore = "requires an isolated TEST_DATABASE_URL"]
    async fn agent_memory_is_scoped_bounded_and_revocation_aware() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let identities = IdentityService::new(db.clone());
        let registry = AgentRegistry::new(db.clone());
        let user:Uuid=sqlx::query_scalar("INSERT INTO users(profile_facts) VALUES('{\"global_secret\":\"must not leak\"}') RETURNING id").fetch_one(db.pool()).await.unwrap();
        let other: Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let a = identities.resolve_for_user(user).await.unwrap();
        let b = identities.resolve_for_user(other).await.unwrap();
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
                    instructions: "Keep specialist work separate.".into(),
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
                json!({"personal":"default-only fact"})
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
                json!({"engineering":"specialist-only fact"})
                    .as_object()
                    .unwrap()
                    .clone(),
            )
            .await
            .unwrap();
        assert!(memory.load(b.owner(), &specialist).await.is_err());
        assert!(
            memory
                .update_facts(
                    b.owner(),
                    &specialist,
                    json!({"leak":true}).as_object().unwrap().clone()
                )
                .await
                .is_err()
        );
        let conv:Uuid=sqlx::query_scalar("INSERT INTO conversations(user_id,user_context_id,agent_external_key,channel,external_conversation_id) VALUES($1,$2,$3,'test',$4) RETURNING id")
            .bind(user).bind(a.id.0).bind(&specialist).bind(Uuid::new_v4().to_string()).fetch_one(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO messages(conversation_id,sequence_number,role,text) VALUES($1,1,'user','specialist-only message')").bind(conv).execute(db.pool()).await.unwrap();
        crate::summaries::handler::SummaryHandler::new(db.clone(), Arc::new(SummaryFixture))
            .handle(crate::conversations::ConversationId(conv))
            .await
            .unwrap();
        let global: serde_json::Value =
            sqlx::query_scalar("SELECT profile_facts FROM users WHERE id=$1")
                .bind(user)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(global, json!({"global_secret":"must not leak"}));
        let ordinary = memory.load(a.owner(), &default).await.unwrap();
        assert!(ordinary.contains("default-only fact"));
        assert!(!ordinary.contains("specialist-only"));
        assert!(!ordinary.contains("global_secret"));
        let private = memory.load(a.owner(), &specialist).await.unwrap();
        assert!(private.contains("specialist-only summary"));
        assert!(!private.contains("default-only"));
        let foreign = memory.load(b.owner(), &default).await.unwrap();
        assert!(!foreign.contains("default-only"));
        // No cached projection can conceal archive, template withdrawal or later changes.
        registry
            .mutate_owned(
                &a,
                AgentMutation::Archive {
                    agent_key: specialist.clone(),
                },
            )
            .await
            .unwrap();
        assert!(memory.load(a.owner(), &specialist).await.is_err());
        assert!(
            memory
                .update_facts(
                    a.owner(),
                    &specialist,
                    json!({"after":"archive"}).as_object().unwrap().clone()
                )
                .await
                .is_err()
        );
        assert!(
            memory
                .update_facts(
                    a.owner(),
                    &default,
                    json!({"oversize":"x".repeat(9000)})
                        .as_object()
                        .unwrap()
                        .clone()
                )
                .await
                .is_err()
        );
        for _ in 0..5 {
            sqlx::query("INSERT INTO conversations(user_id,user_context_id,agent_external_key,channel,external_conversation_id,latest_summary,summary_version) VALUES($1,$2,$3,'test',$4,$5,1)")
                .bind(user).bind(a.id.0).bind(&default).bind(Uuid::new_v4().to_string())
                .bind(json!({"recap":"😀".repeat(4000),"decisions":["d".repeat(7000)],"commitments":["c".repeat(7000)]})).execute(db.pool()).await.unwrap();
        }
        let bounded = memory.load(a.owner(), &default).await.unwrap();
        assert!(bounded.len() <= projection::MAX_CONTEXT_BYTES);
        serde_json::from_str::<serde_json::Value>(&bounded).unwrap();
        assert!(bounded.contains("default-only fact"));
        let cleared = memory
            .manage(a.owner(), &default, MemoryOperation::Clear)
            .await
            .unwrap();
        assert!(cleared.retention_enabled);
        assert_eq!(cleared.retained["facts"], json!({}));
        assert_eq!(cleared.retained["recent_recaps"], json!([]));
        // Conversation history survives; clearing memory does not erase task evidence.
        assert!(
            sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM conversations WHERE id=$1)")
                .bind(conv)
                .fetch_one(db.pool())
                .await
                .unwrap()
        );
        let off = memory
            .manage(
                a.owner(),
                &default,
                MemoryOperation::SetRetention { enabled: false },
            )
            .await
            .unwrap();
        assert!(!off.retention_enabled);
        assert!(
            memory
                .update_facts(
                    a.owner(),
                    &default,
                    json!({"hidden":"must not retain"})
                        .as_object()
                        .unwrap()
                        .clone()
                )
                .await
                .is_err()
        );
        memory
            .manage(
                a.owner(),
                &default,
                MemoryOperation::SetRetention { enabled: true },
            )
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(
                &memory.load(a.owner(), &default).await.unwrap()
            )
            .unwrap()["recent_recaps"],
            json!([])
        );
        memory
            .update_facts(
                a.owner(),
                &default,
                json!({"new":"after clear"}).as_object().unwrap().clone(),
            )
            .await
            .unwrap();
        // An identical setting retry cannot delete newly retained facts.
        let retry = memory
            .manage(
                a.owner(),
                &default,
                MemoryOperation::SetRetention { enabled: true },
            )
            .await
            .unwrap();
        assert_eq!(retry.retained["facts"], json!({"new":"after clear"}));
        assert!(
            memory
                .manage(b.owner(), &specialist, MemoryOperation::Clear)
                .await
                .is_err()
        );
    }
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum MemoryOperation {
    Read,
    Clear,
    SetRetention { enabled: bool },
}

#[derive(Debug, serde::Serialize)]
pub struct AgentMemoryView {
    pub retention_enabled: bool,
    pub cleared_at: chrono::DateTime<chrono::Utc>,
    pub retained: serde_json::Value,
}

impl MemoryService {
    pub async fn manage(
        &self,
        owner: ResourceOwner,
        agent_key: &str,
        operation: MemoryOperation,
    ) -> Result<AgentMemoryView, sqlx::Error> {
        let mut tx = self.db.pool().begin().await?;
        let actor = active_actor(&mut tx, owner, agent_key).await?;
        match operation {
            MemoryOperation::Read => {}
            MemoryOperation::Clear => {
                sqlx::query("INSERT INTO agent_memories(user_context_id,agent_id,cleared_at) VALUES($1,$2,now()) ON CONFLICT(user_context_id,agent_id) DO UPDATE SET facts='{}',cleared_at=now(),version=agent_memories.version+1,updated_at=now()")
                    .bind(owner.user_context_id.0).bind(actor).execute(&mut *tx).await?;
            }
            MemoryOperation::SetRetention { enabled } => {
                // Begin a new retention window, never resurface summaries from a disabled period.
                sqlx::query("INSERT INTO agent_memories(user_context_id,agent_id,retention_enabled,cleared_at) VALUES($1,$2,$3,now()) ON CONFLICT(user_context_id,agent_id) DO UPDATE SET retention_enabled=EXCLUDED.retention_enabled,facts='{}',cleared_at=now(),version=agent_memories.version+1,updated_at=now() WHERE agent_memories.retention_enabled<>EXCLUDED.retention_enabled")
                    .bind(owner.user_context_id.0).bind(actor).bind(enabled).execute(&mut *tx).await?;
            }
        }
        let settings=sqlx::query_as::<_,(bool,chrono::DateTime<chrono::Utc>)>("SELECT retention_enabled,cleared_at FROM agent_memories WHERE user_context_id=$1 AND agent_id=$2")
            .bind(owner.user_context_id.0).bind(actor).fetch_optional(&mut *tx).await?
            .unwrap_or((true,chrono::DateTime::UNIX_EPOCH));
        tx.commit().await?;
        let retained = serde_json::from_str(&self.load(owner, agent_key).await?)
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        Ok(AgentMemoryView {
            retention_enabled: settings.0,
            cleared_at: settings.1,
            retained,
        })
    }
}
