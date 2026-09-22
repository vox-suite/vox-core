/**
* Context cache, memory projection, and user name resolution.
*/
pub mod cache;
pub mod greetings;
pub mod projection;

use crate::{
    db::Db,
    identity::UserId,
};
use std::sync::Arc;

#[derive(Clone)]
pub struct MemoryService {
    db: Db,
    cache: Option<Arc<dyn cache::ContextCache>>,
}

impl MemoryService {
    pub fn new(db: Db, cache: Option<Arc<dyn cache::ContextCache>>) -> Self {
        Self {
            db,
            cache,
        }
    }

    pub fn cache(&self) -> Option<&Arc<dyn cache::ContextCache>> {
        self.cache.as_ref()
    }

    pub async fn load(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(value)) = cache.get(user_id).await
        {
            return Ok(value);
        }
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
        }
        Ok(value)
    }

    pub async fn get_user_name(&self, user_id: UserId) -> Result<Option<String>, sqlx::Error> {
        if let Some(cache) = &self.cache
            && let Ok(Some(name)) = cache.get_user_name(user_id).await
        {
            return Ok(Some(name));
        }

        let name: Option<String> =
            sqlx::query_scalar("SELECT facts->>\x27name\x27 FROM user_profiles WHERE user_id = $1")
                .bind(user_id.0)
                .fetch_optional(self.db.pool())
                .await?
                .flatten();

        if let Some(ref n) = name
            && let Some(cache) = &self.cache
        {
            let _ = cache.set_user_name(user_id, n).await;
            let _ = cache.set_user_id_by_name(n, user_id).await;
        }

        Ok(name)
    }

    pub async fn set_user_name(&self, user_id: UserId, name: &str) -> Result<(), sqlx::Error> {
        let trimmed = name.trim();
        sqlx::query(
            "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
             VALUES ($1, jsonb_build_object(\x27name\x27, $2::text), 1, now()) \
             ON CONFLICT (user_id) DO UPDATE SET \
             facts = jsonb_set(user_profiles.facts, \x27{name}\x27, to_jsonb($2::text), true), \
             updated_at = now()",
        )
        .bind(user_id.0)
        .bind(trimmed)
        .execute(self.db.pool())
        .await?;

        if let Some(cache) = &self.cache {
            let _ = cache.set_user_name(user_id, trimmed).await;
            let _ = cache.set_user_id_by_name(trimmed, user_id).await;
            if let Ok(identities) = sqlx::query_as::<_, (String, String)>(
                "SELECT channel, external_id FROM user_identities WHERE user_id = $1",
            )
            .bind(user_id.0)
            .fetch_all(self.db.pool())
            .await
            {
                for (channel, external_id) in identities {
                    let _ = cache
                        .set_greeting_name(&channel, &external_id, trimmed)
                        .await;
                }
            }
        }

        Ok(())
    }

    pub async fn find_user_by_name(&self, name: &str) -> Result<Option<UserId>, sqlx::Error> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        if let Some(cache) = &self.cache
            && let Ok(Some(uid)) = cache.get_user_id_by_name(trimmed).await
        {
            return Ok(Some(uid));
        }

        let user_id = sqlx::query_scalar::<_, uuid::Uuid>(
            "SELECT user_id FROM user_profiles \
             WHERE LOWER(facts->>\x27name\x27) = LOWER($1) \
             LIMIT 1",
        )
        .bind(trimmed)
        .fetch_optional(self.db.pool())
        .await?;

        let uid = user_id.map(UserId);
        if let Some(uid) = uid
            && let Some(cache) = &self.cache
        {
            let _ = cache.set_user_id_by_name(trimmed, uid).await;
        }
        Ok(uid)
    }

    pub async fn get_verification_state(&self, conversation_id: uuid::Uuid) -> Option<String> {
        if let Some(cache) = &self.cache
            && let Ok(Some(state)) = cache.get_verification_state(conversation_id).await
        {
            return Some(state);
        }
        None
    }

    pub async fn set_verification_state(&self, conversation_id: uuid::Uuid, state: &str) {
        if let Some(cache) = &self.cache {
            let _ = cache.set_verification_state(conversation_id, state).await;
        }
    }

    pub async fn clear_verification_state(&self, conversation_id: uuid::Uuid) {
        if let Some(cache) = &self.cache {
            let _ = cache.clear_verification_state(conversation_id).await;
        }
    }

    pub async fn refresh(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
        }
        Ok(value)
    }

}
