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

    pub async fn refresh(&self, user_id: UserId) -> Result<(), sqlx::Error> {
        let value = projection::build(&self.db, user_id).await?;
        if let Some(cache) = &self.cache {
            let _ = cache.set(user_id, &value).await;
        }
        Ok(())
    }
}
