/**
 * Minimal user and recent-recap Redis sync from PostgreSQL.
 */
use super::{
    MemoryService,
    cache::{CacheError, MinimalChannel, MinimalUserInfo, Recap, device_kinds},
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

        let mut platforms: HashMap<Uuid, Vec<String>> = HashMap::new();
        for (user_id, platform) in sqlx::query_as::<_, (Uuid, String)>(
            "SELECT user_id, platform FROM devices WHERE revoked_at IS NULL",
        )
        .fetch_all(self.db.pool())
        .await?
        {
            platforms.entry(user_id).or_default().push(platform);
        }
        let mobile_consent: std::collections::HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
            "SELECT DISTINCT user_id FROM data_source_consents \
             WHERE granted_at IS NOT NULL AND revoked_at IS NULL",
        )
        .fetch_all(self.db.pool())
        .await?
        .into_iter()
        .collect();

        let users: Vec<(UserId, MinimalUserInfo)> = by_user
            .into_iter()
            .map(|(id, info)| {
                let devices = device_kinds(
                    platforms.get(&id).into_iter().flatten().map(String::as_str),
                    mobile_consent.contains(&id),
                );
                (
                    UserId(id),
                    MinimalUserInfo::new(info.name, info.channels, devices),
                )
            })
            .collect();
        let count = users.len();
        cache.replace_users(&users).await?;

        // Last 5 summarized conversations per user, general agent only so specialist
        // agents' content never surfaces in a user-wide entry.
        let mut recaps: HashMap<Uuid, Vec<Recap>> = HashMap::new();
        for (user_id, recap, updated_at) in
            sqlx::query_as::<_, (Uuid, String, chrono::DateTime<chrono::Utc>)>(
                "SELECT user_id, recap, updated_at FROM ( \
                 SELECT user_id, latest_summary->>'recap' AS recap, updated_at, \
                        ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY updated_at DESC) AS rn \
                 FROM conversations \
                 WHERE summary_version > 0 AND agent_external_key = 'general' \
             ) ranked WHERE rn <= 5 AND recap IS NOT NULL AND recap <> '' \
             ORDER BY user_id, updated_at DESC",
            )
            .fetch_all(self.db.pool())
            .await?
        {
            recaps
                .entry(user_id)
                .or_default()
                .push(Recap { recap, updated_at });
        }
        let recaps: Vec<(UserId, Vec<Recap>)> = recaps
            .into_iter()
            .map(|(id, entries)| (UserId(id), entries))
            .collect();
        cache.replace_recaps(&recaps).await?;
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
                    Duration::from_secs(600)
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
