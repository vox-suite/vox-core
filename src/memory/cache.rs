/**
 * Redis-backed minimal user cache (name, phone, device types, channels).
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    /// Kinds of linked devices: "desktop", "mobile".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub devices: Vec<String>,
    /// Connected services; not populated yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub connections: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub channels: Vec<MinimalChannel>,
}

impl MinimalUserInfo {
    pub fn new(name: Option<String>, channels: Vec<MinimalChannel>, devices: Vec<String>) -> Self {
        let name = name.map(|value| value.trim().to_owned()).filter(|value| !value.is_empty());
        let phone = channels
            .iter()
            .find(|c| c.channel.eq_ignore_ascii_case("phone"))
            .map(|c| c.external_id.clone());
        let (first_name, last_name) = split_name(name.as_deref());
        Self {
            name,
            first_name,
            last_name,
            phone,
            devices,
            connections: Vec::new(),
            channels,
        }
    }
}

fn split_name(name: Option<&str>) -> (Option<String>, Option<String>) {
    let mut parts = name.unwrap_or_default().split_whitespace();
    let first = parts.next().map(str::to_owned);
    let rest = parts.collect::<Vec<_>>().join(" ");
    (first, (!rest.is_empty()).then_some(rest))
}

/// Maps device platforms (and mobile-only data consents) to the kinds shown in the cache.
pub fn device_kinds<'a>(
    platforms: impl IntoIterator<Item = &'a str>,
    has_mobile_consent: bool,
) -> Vec<String> {
    let mut kinds = std::collections::BTreeSet::new();
    for platform in platforms {
        let p = platform.to_ascii_lowercase();
        if p.contains("android") || p.contains("ios") {
            kinds.insert("mobile");
        } else {
            kinds.insert("desktop");
        }
    }
    if has_mobile_consent {
        kinds.insert("mobile");
    }
    kinds.into_iter().map(str::to_owned).collect()
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

    async fn delete_user(&self, _user_id: UserId) -> Result<(), CacheError> {
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
        if info.name.is_none() && info.phone.is_none() && info.devices.is_empty() && info.connections.is_empty() && info.channels.is_empty() {
            connection.del::<_, ()>(redis_keys::user(user_id)).await?;
            return Ok(());
        }
        let payload = serde_json::to_string(info).map_err(|_| CacheError::Payload)?;
        let mut pipeline = redis::pipe();
        pipeline
            .atomic()
            .set_ex(redis_keys::user(user_id), payload, 86400)
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

    async fn delete_user(&self, user_id: UserId) -> Result<(), CacheError> {
        let mut connection = self.client.get_multiplexed_async_connection().await?;
        let payload: Option<String> = connection.get(redis_keys::user(user_id)).await?;
        if let Some(info) =
            payload.and_then(|raw| serde_json::from_str::<MinimalUserInfo>(&raw).ok())
        {
            for channel in &info.channels {
                let key = redis_keys::user_by_channel(&channel.channel, &channel.external_id);
                let owner: Option<String> = connection.get(&key).await?;
                if owner.as_deref() == Some(user_id.0.to_string().as_str()) {
                    let _: () = connection.del(&key).await?;
                }
            }
        }
        let _: () = connection.del(redis_keys::user(user_id)).await?;
        Ok(())
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
