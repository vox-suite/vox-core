/**
 * Context cache, memory projection, and user name resolution.
 */
pub mod cache;
pub mod greetings;
pub mod projection;

use crate::{db::Db, identity::UserId};
use cache::{MinimalChannel, MinimalUserInfo};
use std::sync::Arc;

#[derive(Clone)]
pub struct MemoryService {
    db: Db,
    cache: Option<Arc<dyn cache::ContextCache>>,
    projection_cache:
        Arc<tokio::sync::RwLock<std::collections::HashMap<UserId, (String, std::time::Instant)>>>,
}

impl MemoryService {
    pub fn new(db: Db, cache: Option<Arc<dyn cache::ContextCache>>) -> Self {
        Self {
            db,
            cache,
            projection_cache: Arc::default(),
        }
    }

    pub fn cache(&self) -> Option<&Arc<dyn cache::ContextCache>> {
        self.cache.as_ref()
    }

    /// Full LLM context projection — cached in-memory with TTL to avoid repeated DB scans during turns.
    pub async fn load(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        if let Some((projection, at)) = self.projection_cache.read().await.get(&user_id)
            && at.elapsed() < std::time::Duration::from_secs(30)
        {
            return Ok(projection.clone());
        }
        let value = projection::build(&self.db, user_id).await?;
        self.projection_cache
            .write()
            .await
            .insert(user_id, (value.clone(), std::time::Instant::now()));
        Ok(value)
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
        self.projection_cache.write().await.remove(&user_id);
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

    pub async fn refresh(&self, user_id: UserId) -> Result<String, sqlx::Error> {
        // Projection is Postgres-only; refresh also rewrites the minimal Redis user.
        let value = projection::build(&self.db, user_id).await?;
        self.projection_cache
            .write()
            .await
            .insert(user_id, (value.clone(), std::time::Instant::now()));
        let _ = self.write_minimal_user(user_id, None).await;
        Ok(value)
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
        let info = MinimalUserInfo { name, channels };
        let _ = cache.put_user(user_id, &info).await;
        Ok(())
    }
}
