use chrono::{DateTime, Utc};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::identity::{
    DeploymentId, HostAppId, HostOrganizationId, ResolvedUserContext, UserContextId,
    UserContextSubject, UserId,
};

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
    #[serde(skip)]
    pub context: Option<ResolvedUserContext>,
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
            context: None,
        }))
    }
    pub async fn active(&self, id: Uuid) -> Result<Option<VoiceBinding>, sqlx::Error> {
        let row = sqlx::query("SELECT s.id,s.user_id,s.device_id,uc.id AS ctx_id,uc.deployment_id,uc.host_app_id,uc.organization_id,uc.host_user_id FROM desktop_voice_sessions s LEFT JOIN LATERAL (SELECT * FROM user_contexts WHERE user_contexts.user_id=s.user_id LIMIT 1) uc ON true WHERE s.id=$1 AND s.state='active' AND s.redeemed_at IS NOT NULL AND s.created_at>now()-interval '12 hours' AND (s.device_id IS NULL OR EXISTS(SELECT 1 FROM devices WHERE devices.id=s.device_id AND devices.user_id=s.user_id AND is_active AND revoked_at IS NULL))").bind(id).fetch_optional(&self.pool).await?;
        Ok(row.map(|r| VoiceBinding {
            session_id: r.get("id"),
            user_id: r.get("user_id"),
            device_id: r.get("device_id"),
            context: r
                .get::<Option<Uuid>, _>("ctx_id")
                .map(|ctx_id| ResolvedUserContext {
                    id: UserContextId(ctx_id),
                    user_id: UserId(r.get("user_id")),
                    subject: UserContextSubject {
                        deployment_id: DeploymentId(r.get("deployment_id")),
                        host_app_id: HostAppId(r.get("host_app_id")),
                        organization_id: r
                            .get::<Option<Uuid>, _>("organization_id")
                            .map(HostOrganizationId),
                        host_user_id: r.get("host_user_id"),
                    },
                }),
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
