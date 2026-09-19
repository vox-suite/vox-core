use crate::db::Db;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UserId(pub Uuid);

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChannelIdentity {
    pub channel: String,
    pub external_id: String,
}

#[derive(Clone)]
pub struct IdentityService {
    db: Db,
}

impl IdentityService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub fn normalize_phone(raw: &str) -> String {
        raw.chars().filter(|c| c.is_ascii_digit()).collect()
    }

    pub async fn resolve(&self, identity: &ChannelIdentity) -> Result<UserId, sqlx::Error> {
        let channel = identity.channel.trim();
        let external_id = identity.external_id.trim();

        // 1. Direct match on (channel, external_id)
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
        )
        .bind(channel)
        .bind(external_id)
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }

        let mut tx = self.db.pool().begin().await?;

        // 2. Cross-channel match: Link Phone and WhatsApp if digits match
        if channel == "phone" || channel == "whatsapp" {
            let normalized = Self::normalize_phone(external_id);
            if !normalized.is_empty() {
                let other_channel = if channel == "whatsapp" {
                    "phone"
                } else {
                    "whatsapp"
                };
                let existing_user = sqlx::query_scalar::<_, Uuid>(
                    "SELECT user_id FROM user_identities \
                     WHERE channel = $1 AND regexp_replace(external_id, '[^0-9]', '', 'g') = $2 \
                     LIMIT 1",
                )
                .bind(other_channel)
                .bind(&normalized)
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(user_id) = existing_user {
                    sqlx::query(
                        "INSERT INTO user_identities (user_id, channel, external_id) \
                         VALUES ($1, $2, $3) \
                         ON CONFLICT (channel, external_id) DO NOTHING",
                    )
                    .bind(user_id)
                    .bind(channel)
                    .bind(external_id)
                    .execute(&mut *tx)
                    .await?;

                    tx.commit().await?;
                    return Ok(UserId(user_id));
                }
            }
        }

        // 3. New user
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;

        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO NOTHING RETURNING user_id",
        )
        .bind(new_user)
        .bind(channel)
        .bind(external_id)
        .fetch_optional(&mut *tx)
        .await?;

        let id = if let Some(id) = inserted {
            sqlx::query(
                "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
                 VALUES ($1, '{}'::jsonb, 1, now()) \
                 ON CONFLICT (user_id) DO NOTHING",
            )
            .bind(new_user)
            .execute(&mut *tx)
            .await?;
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
            )
            .bind(channel)
            .bind(external_id)
            .fetch_one(&mut *tx)
            .await?
        };

        tx.commit().await?;
        Ok(UserId(id))
    }
}
