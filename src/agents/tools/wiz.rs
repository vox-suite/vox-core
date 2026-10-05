use crate::{db::Db, realtime::DeviceHub};
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum WizToolError {
    #[error("{0}")]
    Unavailable(&'static str),
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Deserialize)]
pub struct WizArgs {
    pub action: String,
    pub device_id: Option<String>,
    pub on: Option<bool>,
    pub brightness: Option<u8>,
    pub color: Option<[u8; 3]>,
    pub temp: Option<u16>,
}

#[derive(Clone)]
pub struct ControlWizLights {
    pub db: Option<Db>,
    pub hub: Option<DeviceHub>,
    pub user_id: Uuid,
}

impl Tool for ControlWizLights {
    const NAME: &'static str = "control_wiz_lights";
    type Args = WizArgs;
    type Output = Value;
    type Error = WizToolError;

    fn description(&self) -> String {
        "List or control the user's Philips WiZ lights through their desktop app on the home network. Use action=list first to get device ids, then action=set with device_id and any of on, brightness (0-100), color as [r,g,b] (0-255) or temp in kelvin (2200 warm to 6500 cool); color and temp cannot be combined. Fails if the desktop app is offline, remote control is off, or WiZ is not connected there. Report success only when the result says ok.".into()
    }

    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
            "action":{"type":"string","enum":["list","set"]},
            "device_id":{"type":"string"},
            "on":{"type":"boolean"},
            "brightness":{"type":"integer","minimum":0,"maximum":100},
            "color":{"type":"array","items":{"type":"integer","minimum":0,"maximum":255},"minItems":3,"maxItems":3},
            "temp":{"type":"integer","minimum":2200,"maximum":6500}
        },"required":["action"]})
    }

    async fn call(
        &self,
        _context: &mut rig::tool::ToolContext,
        args: WizArgs,
    ) -> Result<Value, WizToolError> {
        let db = self.db.as_ref().ok_or(WizToolError::Unavailable("Database not configured"))?;
        let hub = self.hub.as_ref().ok_or(WizToolError::Unavailable("Device link not configured"))?;
        if !matches!(args.action.as_str(), "list" | "set") {
            return Err(WizToolError::Unavailable("action must be list or set"));
        }
        let device: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM devices WHERE user_id = $1 AND is_active = true \
             AND revoked_at IS NULL AND execution_consent = true \
             AND capabilities->>'wiz' = 'true' ORDER BY last_seen_at DESC LIMIT 1",
        )
        .bind(self.user_id)
        .fetch_optional(db.pool())
        .await?;
        let link = device
            .and_then(|id| hub.get(id))
            .ok_or(WizToolError::Unavailable(
                "No connected desktop with WiZ. Open the Vox desktop app at home with remote control on.",
            ))?;
        let response = link
            .request(
                "wiz",
                json!({"action":args.action,"device_id":args.device_id,"on":args.on,"brightness":args.brightness,"color":args.color,"temp":args.temp}),
                Duration::from_secs(10),
            )
            .await
            .map_err(|_| WizToolError::Unavailable("The desktop did not respond."))?;
        Ok(response)
    }
}
