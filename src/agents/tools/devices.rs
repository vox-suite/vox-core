use crate::{
    db::Db,
    identity::{ResourceOwner, UserId},
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};
use uuid::Uuid;

#[derive(Debug)]
pub enum DeviceToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotFound(String),
    NotConfigured,
}

impl fmt::Display for DeviceToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for DeviceToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for DeviceToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListDevicesArgs {}

#[derive(Clone)]
pub struct ListDevices {
    db: Option<Db>,
    user_id: UserId,
}

impl ListDevices {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListDevices {
    const NAME: &'static str = "list_devices";
    type Args = ListDevicesArgs;
    type Output = Value;
    type Error = DeviceToolError;

    fn description(&self) -> String {
        "List the user's active client devices (Mac desktop, iPhone, Android) and their current telemetry (active app, battery, location)."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {}
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(DeviceToolError::NotConfigured)?;
        let rows = sqlx::query(
            "SELECT id, device_identifier, platform, device_name, is_active, last_seen_at, telemetry \
             FROM client_devices \
             WHERE user_id = $1 AND is_active = true \
             ORDER BY last_seen_at DESC",
        )
        .bind(self.user_id.0)
        .fetch_all(db.pool())
        .await?;

        let devices: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let ident: String = r.get("device_identifier");
                let platform: String = r.get("platform");
                let name: String = r.get("device_name");
                let last_seen: chrono::DateTime<chrono::Utc> = r.get("last_seen_at");
                let telemetry: Value = r.get("telemetry");
                json!({
                    "id": id.to_string(),
                    "identifier": ident,
                    "platform": platform,
                    "name": name,
                    "last_seen_at": last_seen.to_rfc3339(),
                    "telemetry": telemetry
                })
            })
            .collect();

        Ok(json!({ "devices": devices }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DispatchDeviceCommandArgs {
    pub target_device: Option<String>,
    pub command_type: String,
    pub command_payload: Option<Value>,
}

#[derive(Clone)]
pub struct DispatchDeviceCommand {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl DispatchDeviceCommand {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for DispatchDeviceCommand {
    const NAME: &'static str = "dispatch_device_command";
    type Args = DispatchDeviceCommandArgs;
    type Output = Value;
    type Error = DeviceToolError;

    fn description(&self) -> String {
        "Dispatch an action to execute directly on the user's client machine (e.g. open terminal, launch app, focus window on their Mac)."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "target_device": {
                    "type": "string",
                    "description": "Optional device platform or name (e.g. 'mac', 'macbook', 'phone'); defaults to most recently active desktop"
                },
                "command_type": {
                    "type": "string",
                    "enum": ["launch_app", "run_terminal", "open_url", "system_control"],
                    "description": "The command action to trigger on client"
                },
                "command_payload": {
                    "type": "object",
                    "description": "Parameters for the command, e.g. {\"app_name\": \"Terminal\"} or {\"url\": \"https://github.com\"}"
                }
            },
            "required": ["command_type"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.owner.user_id.0,
            user_context_id = %self.owner.user_context_id.0,
            command_type = %args.command_type,
            target_device = ?args.target_device,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(DeviceToolError::NotConfigured)?;
        let cmd_type = args.command_type.trim();
        let payload = args.command_payload.unwrap_or_else(|| json!({}));

        let device_target = args.target_device.as_deref().unwrap_or("darwin");

        let device = sqlx::query(
            "SELECT id, device_name, platform FROM client_devices \
             WHERE user_id = $1 AND is_active = true AND (platform ILIKE '%' || $2 || '%' OR device_name ILIKE '%' || $2 || '%') \
             ORDER BY last_seen_at DESC LIMIT 1",
        )
        .bind(self.owner.user_id.0)
        .bind(device_target)
        .fetch_optional(db.pool())
        .await?;

        let device_id: Option<Uuid> = device.as_ref().map(|d| d.get("id"));
        let device_name: String = device
            .as_ref()
            .map(|d| d.get("device_name"))
            .unwrap_or_else(|| "default_mac".into());

        let idempotency_key = format!(
            "client_cmd:{}:{}",
            self.owner.user_context_id.0,
            Uuid::new_v4()
        );

        let action_payload = json!({
            "command_type": cmd_type,
            "target_device_name": device_name,
            "parameters": payload
        });

        let action_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO actions (user_context_id, user_id, target_device_id, kind, payload, state, idempotency_key) \
             VALUES ($1, $2, $3, 'client_command', $4, 'pending', $5) \
             RETURNING id",
        )
        .bind(self.owner.user_context_id.0)
        .bind(self.owner.user_id.0)
        .bind(device_id)
        .bind(&action_payload)
        .bind(&idempotency_key)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "queued",
            "action_id": action_id.to_string(),
            "target_device": device_name,
            "command": cmd_type
        }))
    }
}
