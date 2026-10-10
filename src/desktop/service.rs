use crate::realtime::DeviceHub;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DesktopError {
    #[error("No connected desktop with app control enabled. Open Vox and enable remote control.")]
    Offline,
    #[error("More than one desktop is connected. Ask which device to use.")]
    Ambiguous,
    #[error("Desktop context or action could not be verified")]
    Invalid,
    #[error(
        "Desktop did not acknowledge the action; its outcome is unknown. Do not repeat mutations automatically."
    )]
    Unacknowledged,
    #[error("Desktop database unavailable")]
    Database(#[from] sqlx::Error),
}
#[derive(Clone)]
pub struct DesktopControlService {
    pub pool: PgPool,
    pub hub: DeviceHub,
}
impl DesktopControlService {
    async fn eligible_devices(&self, user: Uuid) -> Result<Vec<Value>, DesktopError> {
        let rows=sqlx::query("SELECT id,label FROM devices WHERE user_id=$1 AND is_active AND revoked_at IS NULL AND execution_consent AND capabilities->>'vox_app_control'='true'").bind(user).fetch_all(&self.pool).await?;
        Ok(rows
            .into_iter()
            .map(|r| json!({"id":r.get::<Uuid,_>("id"),"label":r.get::<String,_>("label")}))
            .collect())
    }
    pub async fn devices(&self, user: Uuid) -> Result<Vec<Value>, DesktopError> {
        Ok(self
            .eligible_devices(user)
            .await?
            .into_iter()
            .filter(|d| {
                d["id"]
                    .as_str()
                    .and_then(|s| Uuid::parse_str(s).ok())
                    .is_some_and(|id| self.hub.get(id).is_some())
            })
            .collect())
    }
    pub async fn target(
        &self,
        user: Uuid,
        requested: Option<Uuid>,
        session: Option<&str>,
    ) -> Result<Uuid, DesktopError> {
        let bound = if let Some(id) = session
            .and_then(|s| s.strip_prefix("desktop:"))
            .and_then(|s| Uuid::parse_str(s).ok())
        {
            sqlx::query_scalar::<_,Option<Uuid>>("SELECT device_id FROM desktop_voice_sessions WHERE id=$1 AND user_id=$2 AND state='active'").bind(id).bind(user).fetch_optional(&self.pool).await?.flatten()
        } else {
            None
        };
        if bound.is_some() && requested.is_some() && bound != requested {
            return Err(DesktopError::Invalid);
        }
        let devices = self.eligible_devices(user).await?;
        match bound.or(requested) {
            Some(id) => {
                if devices
                    .iter()
                    .any(|d| d["id"].as_str() == Some(&id.to_string()))
                {
                    Ok(id)
                } else {
                    Err(DesktopError::Offline)
                }
            }
            None => {
                let online = self.devices(user).await?;
                let choices = if online.is_empty() { &devices } else { &online };
                match choices.as_slice() {
                    [d] => Uuid::parse_str(d["id"].as_str().unwrap_or(""))
                        .map_err(|_| DesktopError::Invalid),
                    [] => Err(DesktopError::Offline),
                    _ => Err(DesktopError::Ambiguous),
                }
            }
        }
    }
    pub async fn context(&self, id: Uuid) -> Result<Value, DesktopError> {
        let link = self.hub.get(id).ok_or(DesktopError::Offline)?;
        let result = link
            .request("app_context", json!({}), Duration::from_secs(5))
            .await
            .map_err(|_| DesktopError::Unacknowledged)?;
        if result["ok"] != true {
            return Err(DesktopError::Invalid);
        }
        Ok(result["context"].clone())
    }
    pub async fn dispatch(
        &self,
        id: Uuid,
        action: &str,
        args: Value,
        expected: Option<u64>,
        command_id: Uuid,
    ) -> Result<Value, DesktopError> {
        let link = self.hub.get(id).ok_or(DesktopError::Offline)?;
        let command = json!({"commandId":command_id,"expiresAt":(chrono::Utc::now()+chrono::Duration::seconds(30)).to_rfc3339(),"expectedRevision":expected,"action":action,"arguments":args});
        link.request(
            "app_command",
            json!({"command":command}),
            Duration::from_secs(25),
        )
        .await
        .map_err(|_| DesktopError::Unacknowledged)
    }
}
