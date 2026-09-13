use crate::identity::UserId;
use async_trait::async_trait;
use redis::AsyncCommands;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("context cache unavailable")]
    Redis(#[from] redis::RedisError),
}

#[async_trait]
pub trait ContextCache: Send + Sync {
    async fn get(&self, user_id: UserId) -> Result<Option<String>, CacheError>;
    async fn set(&self, user_id: UserId, value: &str) -> Result<(), CacheError>;
}

pub struct RedisContextCache {
    client: redis::Client,
}

impl RedisContextCache {
    pub fn new(url: &str) -> Result<Self, CacheError> {
        Ok(Self {
            client: redis::Client::open(url)?,
        })
    }
}

#[async_trait]
impl ContextCache for RedisContextCache {
    async fn get(&self, user_id: UserId) -> Result<Option<String>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .get(format!("vox:user-context:{}", user_id.0))
            .await
            .map_err(Into::into)
    }

    async fn set(&self, user_id: UserId, value: &str) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .set(format!("vox:user-context:{}", user_id.0), value)
            .await
            .map_err(Into::into)
    }
}
