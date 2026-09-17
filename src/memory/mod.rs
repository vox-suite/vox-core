pub mod cache;
pub mod projection;

use crate::{db::Db, identity::UserId};
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

        let name: Option<String> = sqlx::query_scalar(
            "SELECT facts->>'name' FROM user_profiles WHERE user_id = $1",
        )
        .bind(user_id.0)
        .fetch_optional(self.db.pool())
        .await?
        .flatten();

        if let Some(ref n) = name {
            if let Some(cache) = &self.cache {
                let _ = cache.set_user_name(user_id, n).await;
            }
        }

        Ok(name)
    }

    pub async fn set_user_name(&self, user_id: UserId, name: &str) -> Result<(), sqlx::Error> {
        let trimmed = name.trim();
        sqlx::query(
            "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
             VALUES ($1, jsonb_build_object('name', $2::text), 1, now()) \
             ON CONFLICT (user_id) DO UPDATE SET \
             facts = jsonb_set(user_profiles.facts, '{name}', to_jsonb($2::text), true), \
             updated_at = now()",
        )
        .bind(user_id.0)
        .bind(trimmed)
        .execute(self.db.pool())
        .await?;

        if let Some(cache) = &self.cache {
            let _ = cache.set_user_name(user_id, trimmed).await;
        }

        Ok(())
    }

    pub async fn refresh(&self, user_id: UserId) -> Result<(), sqlx::Error> {
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
            if let Ok(Some(name)) = sqlx::query_scalar::<_, Option<String>>(
                "SELECT facts->>'name' FROM user_profiles WHERE user_id = $1",
            )
            .bind(user_id.0)
            .fetch_one(self.db.pool())
            .await
            && let Some(name_str) = name
            {
                let _ = cache.set_user_name(user_id, &name_str).await;
            }
        }
        Ok(())
    }
}
