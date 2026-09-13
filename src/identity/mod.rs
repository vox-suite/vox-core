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

    pub async fn resolve(&self, identity: &ChannelIdentity) -> Result<UserId, sqlx::Error> {
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
        )
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(self.db.pool())
        .await?
        {
            return Ok(UserId(id));
        }
        let mut tx = self.db.pool().begin().await?;
        let new_user =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_identities (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO NOTHING RETURNING user_id",
        )
        .bind(new_user)
        .bind(identity.channel.trim())
        .bind(identity.external_id.trim())
        .fetch_optional(&mut *tx)
        .await?;
        let id = if let Some(id) = inserted {
            id
        } else {
            sqlx::query("DELETE FROM users WHERE id = $1")
                .bind(new_user)
                .execute(&mut *tx)
                .await?;
            sqlx::query_scalar::<_, Uuid>(
                "SELECT user_id FROM user_identities WHERE channel = $1 AND external_id = $2",
            )
            .bind(identity.channel.trim())
            .bind(identity.external_id.trim())
            .fetch_one(&mut *tx)
            .await?
        };
        tx.commit().await?;
        Ok(UserId(id))
    }
}
