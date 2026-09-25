/**
 * Minimal user Redis sync from PostgreSQL.
 */
use super::{
    MemoryService,
    cache::{CacheError, MinimalChannel, MinimalUserInfo},
};
use crate::identity::UserId;
use std::collections::HashMap;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum GreetingSyncError {
    #[error("greeting source unavailable")]
    Database(#[from] sqlx::Error),
    #[error("greeting cache unavailable")]
    Cache(#[from] CacheError),
}

impl MemoryService {
    /// Rebuild `vox:user:*` + `vox:channel:*` from Postgres.
    pub async fn sync_minimal_users(&self) -> Result<usize, GreetingSyncError> {
        let Some(cache) = self.cache() else {
            return Ok(0);
        };
        let rows = sqlx::query_as::<_, (Uuid, Option<String>, Option<String>, Option<String>)>(
            "SELECT u.id, \
                    COALESCE(NULLIF(u.profile_facts->>'name', ''), u.display_name), \
                    i.channel, \
                    i.normalized_external_id \
             FROM users u \
             LEFT JOIN channel_identities i ON i.user_id = u.id AND i.revoked_at IS NULL",
        )
        .fetch_all(self.db.pool())
        .await?;

        let mut by_user: HashMap<Uuid, MinimalUserInfo> = HashMap::new();
        for (user_id, name, channel, external_id) in rows {
            let entry = by_user.entry(user_id).or_default();
            if entry.name.is_none() {
                entry.name = name
                    .map(|value| value.trim().to_owned())
                    .filter(|value| !value.is_empty());
            }
            if let (Some(channel), Some(external_id)) = (channel, external_id) {
                entry.channels.push(MinimalChannel {
                    channel,
                    external_id,
                });
            }
        }

        let users: Vec<(UserId, MinimalUserInfo)> = by_user
            .into_iter()
            .map(|(id, info)| (UserId(id), info))
            .collect();
        let count = users.len();
        cache.replace_users(&users).await?;
        Ok(count)
    }

    /// Backward-compatible alias used by API startup.
    pub async fn sync_greeting_names(&self) -> Result<usize, GreetingSyncError> {
        self.sync_minimal_users().await
    }

    pub async fn run_greeting_sync(self, cancellation: CancellationToken) {
        if self.cache().is_none() {
            return;
        }
        loop {
            let result = tokio::select! {
                _ = cancellation.cancelled() => return,
                result = tokio::time::timeout(Duration::from_secs(60), self.sync_minimal_users()) => result,
            };
            let delay = match result {
                Ok(Ok(count)) => {
                    tracing::info!(count, "Synced minimal users to Redis");
                    Duration::from_secs(3600)
                }
                _ => {
                    tracing::warn!("Minimal user cache sync failed; retrying in one minute");
                    Duration::from_secs(60)
                }
            };
            tokio::select! {
                _ = cancellation.cancelled() => return,
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }
}
