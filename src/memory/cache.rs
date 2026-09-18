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
    async fn get_user_name(&self, _user_id: UserId) -> Result<Option<String>, CacheError> {
        Ok(None)
    }
    async fn set_user_name(&self, _user_id: UserId, _name: &str) -> Result<(), CacheError> {
        Ok(())
    }
    async fn get_voice_signature(&self, _user_id: UserId) -> Result<Option<String>, CacheError> {
        Ok(None)
    }
    async fn set_voice_signature(&self, _user_id: UserId, _sig: &str) -> Result<(), CacheError> {
        Ok(())
    }
    async fn get_user_id_by_name(&self, _name: &str) -> Result<Option<UserId>, CacheError> {
        Ok(None)
    }
    async fn set_user_id_by_name(&self, _name: &str, _user_id: UserId) -> Result<(), CacheError> {
        Ok(())
    }
    async fn get_verification_state(&self, _conversation_id: uuid::Uuid) -> Result<Option<String>, CacheError> {
        Ok(None)
    }
    async fn set_verification_state(&self, _conversation_id: uuid::Uuid, _state: &str) -> Result<(), CacheError> {
        Ok(())
    }
    async fn clear_verification_state(&self, _conversation_id: uuid::Uuid) -> Result<(), CacheError> {
        Ok(())
    }
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

    async fn get_user_name(&self, user_id: UserId) -> Result<Option<String>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .get(format!("vox:user-name:{}", user_id.0))
            .await
            .map_err(Into::into)
    }

    async fn set_user_name(&self, user_id: UserId, name: &str) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .set(format!("vox:user-name:{}", user_id.0), name)
            .await
            .map_err(Into::into)
    }

    async fn get_voice_signature(&self, user_id: UserId) -> Result<Option<String>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .get(format!("vox:voice-sig:{}", user_id.0))
            .await
            .map_err(Into::into)
    }

    async fn set_voice_signature(&self, user_id: UserId, sig: &str) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .set(format!("vox:voice-sig:{}", user_id.0), sig)
            .await
            .map_err(Into::into)
    }

    async fn get_user_id_by_name(&self, name: &str) -> Result<Option<UserId>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let id_str: Option<String> = connection
            .get(format!("vox:user-by-name:{}", name.trim().to_lowercase()))
            .await
            .map_err(Into::into)?;
        if let Some(s) = id_str {
            if let Ok(uid) = uuid::Uuid::parse_str(&s) {
                return Ok(Some(UserId(uid)));
            }
        }
        Ok(None)
    }

    async fn set_user_id_by_name(&self, name: &str, user_id: UserId) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .set(
                format!("vox:user-by-name:{}", name.trim().to_lowercase()),
                user_id.0.to_string(),
            )
            .await
            .map_err(Into::into)
    }

    async fn get_verification_state(&self, conversation_id: uuid::Uuid) -> Result<Option<String>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .get(format!("vox:verification-state:{}", conversation_id))
            .await
            .map_err(Into::into)
    }

    async fn set_verification_state(&self, conversation_id: uuid::Uuid, state: &str) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .set_ex(format!("vox:verification-state:{}", conversation_id), state, 3600)
            .await
            .map_err(Into::into)
    }

    async fn clear_verification_state(&self, conversation_id: uuid::Uuid) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        connection
            .del(format!("vox:verification-state:{}", conversation_id))
            .await
            .map_err(Into::into)
    }
}
