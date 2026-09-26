use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

use crate::db::Db;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DataSource {
    Sms,
    Location,
}

impl DataSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sms => "sms",
            Self::Location => "location",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ConsentStatus {
    pub granted: bool,
    pub retention_days: i32,
    pub granted_at: Option<DateTime<Utc>>,
    pub synced_until: Option<DateTime<Utc>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConsentError {
    #[error("retention_days must be positive")]
    InvalidRetention,
    #[error("consent storage unavailable")]
    Database(#[from] sqlx::Error),
}

#[derive(Clone)]
pub struct ConsentService {
    db: Db,
}

impl ConsentService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn status(
        &self,
        user_id: Uuid,
        source: DataSource,
    ) -> Result<ConsentStatus, ConsentError> {
        let row = sqlx::query(
            "SELECT granted_at, revoked_at, retention_days, synced_until FROM data_source_consents \
             WHERE user_id = $1 AND data_source = $2",
        )
        .bind(user_id)
        .bind(source.as_str())
        .fetch_optional(self.db.pool())
        .await?;

        Ok(match row {
            Some(row) => {
                let granted_at: Option<DateTime<Utc>> = row.get("granted_at");
                let revoked_at: Option<DateTime<Utc>> = row.get("revoked_at");
                ConsentStatus {
                    granted: granted_at.is_some() && revoked_at.is_none(),
                    retention_days: row.get("retention_days"),
                    granted_at,
                    synced_until: row.get("synced_until"),
                }
            }
            None => ConsentStatus {
                granted: false,
                retention_days: 90,
                granted_at: None,
                synced_until: None,
            },
        })
    }

    pub async fn grant(
        &self,
        user_id: Uuid,
        source: DataSource,
        retention_days: Option<i32>,
    ) -> Result<ConsentStatus, ConsentError> {
        let retention_days = retention_days.unwrap_or(90);
        if retention_days <= 0 {
            return Err(ConsentError::InvalidRetention);
        }

        let row = sqlx::query(
            "INSERT INTO data_source_consents (user_id, data_source, granted_at, revoked_at, retention_days) \
             VALUES ($1, $2, now(), NULL, $3) \
             ON CONFLICT (user_id, data_source) DO UPDATE SET \
                granted_at = now(), revoked_at = NULL, retention_days = $3, updated_at = now() \
             RETURNING granted_at, retention_days",
        )
        .bind(user_id)
        .bind(source.as_str())
        .bind(retention_days)
        .fetch_one(self.db.pool())
        .await?;

        Ok(ConsentStatus {
            granted: true,
            retention_days: row.get("retention_days"),
            granted_at: row.get("granted_at"),
            synced_until: None,
        })
    }

    pub async fn revoke(&self, user_id: Uuid, source: DataSource) -> Result<(), ConsentError> {
        sqlx::query(
            "UPDATE data_source_consents SET revoked_at = now(), updated_at = now() \
             WHERE user_id = $1 AND data_source = $2",
        )
        .bind(user_id)
        .bind(source.as_str())
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    pub async fn is_granted(
        &self,
        user_id: Uuid,
        source: DataSource,
    ) -> Result<bool, ConsentError> {
        Ok(self.status(user_id, source).await?.granted)
    }

    /// Advances the source's server-side sync watermark, never moving it backward.
    pub async fn advance_sync_cursor(
        &self,
        user_id: Uuid,
        source: DataSource,
        until: DateTime<Utc>,
    ) -> Result<(), ConsentError> {
        sqlx::query(
            "UPDATE data_source_consents SET synced_until = GREATEST(COALESCE(synced_until, $3), $3), updated_at = now() \
             WHERE user_id = $1 AND data_source = $2",
        )
        .bind(user_id)
        .bind(source.as_str())
        .bind(until)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }
}
