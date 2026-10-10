use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Clone)]
pub struct DesktopVoiceBroker {
    pool: PgPool,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct IssuedSession {
    pub session_id: Uuid,
    pub ticket: String,
    pub expires_at: DateTime<Utc>,
}
#[derive(Clone, Serialize)]
pub struct VoiceBinding {
    pub session_id: Uuid,
    pub user_id: Uuid,
    pub device_id: Option<Uuid>,
}
impl DesktopVoiceBroker {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn issue(
        &self,
        user: Uuid,
        device: Option<Uuid>,
    ) -> Result<IssuedSession, sqlx::Error> {
        if let Some(device) = device {
            let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM devices WHERE id=$1 AND user_id=$2 AND is_active AND revoked_at IS NULL)").bind(device).bind(user).fetch_one(&self.pool).await?;
            if !owned {
                return Err(sqlx::Error::RowNotFound);
            }
        }
        let id = Uuid::new_v4();
        let ticket = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let hash = hex::encode(Sha256::digest(ticket.as_bytes()));
        let expires_at = sqlx::query_scalar("INSERT INTO desktop_voice_sessions(id,user_id,device_id,ticket_hash,expires_at) VALUES($1,$2,$3,$4,now()+interval '60 seconds') RETURNING expires_at").bind(id).bind(user).bind(device).bind(hash).fetch_one(&self.pool).await?;
        Ok(IssuedSession {
            session_id: id,
            ticket,
            expires_at,
        })
    }
    pub async fn redeem(&self, ticket: &str) -> Result<Option<VoiceBinding>, sqlx::Error> {
        if ticket.len() != 64 {
            return Ok(None);
        }
        let hash = hex::encode(Sha256::digest(ticket.as_bytes()));
        let row = sqlx::query("UPDATE desktop_voice_sessions SET redeemed_at=now() WHERE ticket_hash=$1 AND redeemed_at IS NULL AND expires_at>now() AND state='active' AND (device_id IS NULL OR EXISTS(SELECT 1 FROM devices WHERE devices.id=desktop_voice_sessions.device_id AND devices.user_id=desktop_voice_sessions.user_id AND is_active AND revoked_at IS NULL)) RETURNING id,user_id,device_id").bind(hash).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| VoiceBinding {
            session_id: r.get("id"),
            user_id: r.get("user_id"),
            device_id: r.get("device_id"),
        }))
    }
    pub async fn active(&self, id: Uuid) -> Result<Option<VoiceBinding>, sqlx::Error> {
        let row = sqlx::query("SELECT id,user_id,device_id FROM desktop_voice_sessions WHERE id=$1 AND state='active' AND redeemed_at IS NOT NULL AND created_at>now()-interval '12 hours' AND (device_id IS NULL OR EXISTS(SELECT 1 FROM devices WHERE devices.id=desktop_voice_sessions.device_id AND devices.user_id=desktop_voice_sessions.user_id AND is_active AND revoked_at IS NULL))").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| VoiceBinding {
            session_id: r.get("id"),
            user_id: r.get("user_id"),
            device_id: r.get("device_id"),
        }))
    }
    pub async fn complete(&self, id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE desktop_voice_sessions SET state='completed' WHERE id=$1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }
}
