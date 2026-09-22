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
            sqlx::query_scalar(
                "SELECT COALESCE(NULLIF(profile_facts->>'name', ''), display_name) FROM users WHERE id = $1",
            )
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

        if let Some(cache) = &self.cache {
            let _ = cache.set_user_name(user_id, trimmed).await;
            let _ = cache.set_user_id_by_name(trimmed, user_id).await;
            if let Ok(identities) = sqlx::query_as::<_, (String, String)>(
                "SELECT channel, normalized_external_id FROM channel_identities WHERE user_id = $1 AND revoked_at IS NULL",
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
            "SELECT id FROM users \
             WHERE LOWER(COALESCE(NULLIF(profile_facts->>'name', ''), display_name, '')) = LOWER($1) \
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

    pub async fn refresh(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
        }
        Ok(value)
    }

}
