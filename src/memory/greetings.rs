/**
* Personalized user greetings and sync management.
*/
use super::{MemoryService, cache::CacheError};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum GreetingSyncError {
    #[error("greeting source unavailable")]
    Database(#[from] sqlx::Error),
    #[error("greeting cache unavailable")]
    Cache(#[from] CacheError),
}

impl MemoryService {
    pub async fn sync_greeting_names(&self) -> Result<usize, GreetingSyncError> {
        let Some(cache) = self.cache() else {
            return Ok(0);
        };
        let names = sqlx::query_as::<_, (String, String, String)>(
            "SELECT i.channel, i.normalized_external_id, COALESCE(NULLIF(u.profile_facts->>'name', ''), u.display_name) \
             FROM channel_identities i JOIN users u ON u.id = i.user_id \
             WHERE i.revoked_at IS NULL \
               AND COALESCE(NULLIF(u.profile_facts->>'name', ''), u.display_name) IS NOT NULL",
        )
        .fetch_all(self.db.pool())
        .await?;
        cache.replace_greeting_names(&names).await?;
        Ok(names.len())
    }

    pub async fn run_greeting_sync(self, cancellation: CancellationToken) {
        if self.cache().is_none() {
            return;
        }
        loop {
            let result = tokio::select! {
                _ = cancellation.cancelled() => return,
                result = tokio::time::timeout(Duration::from_secs(60), self.sync_greeting_names()) => result,
            };
            let delay = match result {
                Ok(Ok(count)) => {
                    tracing::info!(count, "Synced greeting names to Redis");
                    Duration::from_secs(86400)
                }
                _ => {
                    tracing::warn!("Greeting cache sync failed; retrying in one minute");
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
