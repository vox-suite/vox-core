/**
 * Redis-backed minimal user cache (name + channels only).
 */
use crate::{identity::UserId, redis_keys};
use async_trait::async_trait;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("context cache unavailable")]
    Redis(#[from] redis::RedisError),
    #[error("context cache payload is invalid")]
    Payload,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct MinimalUserInfo {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<MinimalChannel>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MinimalChannel {
    pub channel: String,
    pub external_id: String,
}

#[async_trait]
pub trait ContextCache: Send + Sync {
    async fn get_user(&self, _user_id: UserId) -> Result<Option<MinimalUserInfo>, CacheError> {
        Ok(None)
    }

    async fn put_user(&self, _user_id: UserId, _info: &MinimalUserInfo) -> Result<(), CacheError> {
        Ok(())
    }

    /// Resolve a caller by channel + external id (e.g. phone number).
    async fn get_user_by_channel(
        &self,
        _channel: &str,
        _external_id: &str,
    ) -> Result<Option<(UserId, MinimalUserInfo)>, CacheError> {
        Ok(None)
    }

    /// Replace the entire minimal-user cache from a Postgres snapshot.
    async fn replace_users(&self, _users: &[(UserId, MinimalUserInfo)]) -> Result<(), CacheError> {
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

    async fn delete_pattern(
        connection: &mut redis::aio::MultiplexedConnection,
        pattern: &str,
    ) -> Result<(), CacheError> {
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(pattern)
                .arg("COUNT")
                .arg(200)
                .query_async(connection)
                .await?;
            if !keys.is_empty() {
                let _: () = connection.del(keys).await?;
            }
            cursor = next;
            if cursor == 0 {
                break;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl ContextCache for RedisContextCache {
    async fn get_user(&self, user_id: UserId) -> Result<Option<MinimalUserInfo>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let payload: Option<String> = connection.get(redis_keys::user(user_id)).await?;
        match payload {
            Some(raw) => Ok(Some(
                serde_json::from_str(&raw).map_err(|_| CacheError::Payload)?,
            )),
            None => Ok(None),
        }
    }

    async fn put_user(&self, user_id: UserId, info: &MinimalUserInfo) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let payload = serde_json::to_string(info).map_err(|_| CacheError::Payload)?;
        let mut pipeline = redis::pipe();
        pipeline
            .atomic()
            .set(redis_keys::user(user_id), payload)
            .ignore();
        for channel in &info.channels {
            pipeline
                .set(
                    redis_keys::user_by_channel(&channel.channel, &channel.external_id),
                    user_id.0.to_string(),
                )
                .ignore();
        }
        pipeline
            .query_async::<()>(&mut connection)
            .await
            .map_err(Into::into)
    }

    async fn get_user_by_channel(
        &self,
        channel: &str,
        external_id: &str,
    ) -> Result<Option<(UserId, MinimalUserInfo)>, CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let user_id: Option<String> = connection
            .get(redis_keys::user_by_channel(channel, external_id))
            .await?;
        let Some(user_id) = user_id else {
            return Ok(None);
        };
        let user_id = Uuid::parse_str(&user_id)
            .map(UserId)
            .map_err(|_| CacheError::Payload)?;
        let info = self.get_user(user_id).await?;
        Ok(info.map(|info| (user_id, info)))
    }

    async fn replace_users(&self, users: &[(UserId, MinimalUserInfo)]) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        Self::delete_pattern(&mut connection, redis_keys::USER_SCAN).await?;
        Self::delete_pattern(&mut connection, redis_keys::CHANNEL_SCAN).await?;
        if users.is_empty() {
            return Ok(());
        }
        let mut pipeline = redis::pipe();
        pipeline.atomic();
        for (user_id, info) in users {
            let payload = serde_json::to_string(info).map_err(|_| CacheError::Payload)?;
            pipeline.set(redis_keys::user(*user_id), payload).ignore();
            for channel in &info.channels {
                pipeline
                    .set(
                        redis_keys::user_by_channel(&channel.channel, &channel.external_id),
                        user_id.0.to_string(),
                    )
                    .ignore();
            }
        }
        pipeline
            .query_async::<()>(&mut connection)
            .await
            .map_err(Into::into)
    }
}
