/**
* Storage repository for registered consumer devices and credentials.
*/
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::devices::Device;

#[derive(Clone)]
pub struct DeviceRepository {
    pool: PgPool,
}

impl DeviceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn register(
        &self,
        user_id: Uuid,
        device_identifier: &str,
        platform: &str,
        label: &str,
        public_key: Option<&str>,
        capabilities: serde_json::Value,
        execution_consent: bool,
    ) -> Result<Device, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO devices (
                user_id, device_identifier, platform, label, public_key, capabilities, execution_consent
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (user_id, device_identifier) DO UPDATE
            SET platform = EXCLUDED.platform,
                label = EXCLUDED.label,
                public_key = EXCLUDED.public_key,
                capabilities = EXCLUDED.capabilities,
                execution_consent = EXCLUDED.execution_consent,
                last_seen_at = now(),
                updated_at = now()
            RETURNING id, user_id, device_identifier, platform, label, public_key,
                      capabilities, execution_consent, is_active, last_seen_at, revoked_at,
                      created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(device_identifier)
        .bind(platform)
        .bind(label)
        .bind(public_key)
        .bind(capabilities)
        .bind(execution_consent)
        .fetch_one(&self.pool)
        .await?;

        Ok(Device {
            id: row.get("id"),
            user_id: row.get("user_id"),
            device_identifier: row.get("device_identifier"),
            platform: row.get("platform"),
            label: row.get("label"),
            public_key: row.get("public_key"),
            capabilities: row.get("capabilities"),
            execution_consent: row.get("execution_consent"),
            is_active: row.get("is_active"),
            last_seen_at: row.get("last_seen_at"),
            revoked_at: row.get("revoked_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn heartbeat(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE devices
            SET last_seen_at = now(), updated_at = now()
            WHERE id = $1 AND user_id = $2 AND is_active = true
            "#,
        )
        .bind(id)
        .bind(user_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }
}
