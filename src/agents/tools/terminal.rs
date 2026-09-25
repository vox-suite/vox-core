/**
* Agent tools for opening and driving a live terminal session on one of
* the user's registered devices over its real-time device connection.
*/
use crate::{
    db::Db,
    identity::UserId,
    realtime::{DeviceHub, DeviceLinkError},
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

const OPEN_TIMEOUT: Duration = Duration::from_secs(8);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(25);
// ponytail: whole tool output is capped rather than streamed/paginated;
// raise this or add pagination if commands routinely produce more output.
const MAX_OUTPUT_CHARS: usize = 4000;

#[derive(Debug, thiserror::Error)]
pub enum TerminalToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Database not configured")]
    NotConfigured,
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("{0}")]
    DeviceUnavailable(String),
}

impl From<DeviceLinkError> for TerminalToolError {
    fn from(err: DeviceLinkError) -> Self {
        TerminalToolError::DeviceUnavailable(err.to_string())
    }
}

#[derive(sqlx::FromRow, Clone)]
struct DeviceRow {
    id: Uuid,
    label: String,
    platform: String,
}

async fn resolve_device(
    db: &Db,
    user_id: Uuid,
    hint: Option<&str>,
) -> Result<DeviceRow, TerminalToolError> {
    let rows = sqlx::query_as::<_, DeviceRow>(
        "SELECT id, label, platform FROM devices \
         WHERE user_id = $1 AND is_active = true AND execution_consent = true \
         ORDER BY last_seen_at DESC",
    )
    .bind(user_id)
    .fetch_all(db.pool())
    .await?;

    if rows.is_empty() {
        return Err(TerminalToolError::DeviceUnavailable(
            "No device has remote control enabled. Ask the user to open the Vox desktop app on their Mac and turn on remote control (the Desktop Link tile).".into(),
        ));
    }

    if let Some(hint) = hint {
        let hint_lower = hint.trim().to_lowercase();
        if !hint_lower.is_empty()
            && let Some(matched) = rows.iter().find(|r| {
                r.label.to_lowercase().contains(&hint_lower)
                    || r.platform.to_lowercase().contains(&hint_lower)
            })
        {
            return Ok(matched.clone());
        }
    }

    if rows.len() == 1 {
        return Ok(rows[0].clone());
    }

    let labels: Vec<String> = rows.iter().map(|r| r.label.clone()).collect();
    Err(TerminalToolError::DeviceUnavailable(format!(
        "The user has multiple registered devices ({}). Ask which one they mean and pass it as device_hint.",
        labels.join(", ")
    )))
}

fn device_link(
    hub: &DeviceHub,
    device: &DeviceRow,
) -> Result<crate::realtime::DeviceLink, TerminalToolError> {
    hub.get(device.id).ok_or_else(|| {
        TerminalToolError::DeviceUnavailable(format!(
            "{} isn't connected right now. Ask the user to make sure the Vox desktop app is open on it.",
            device.label
        ))
    })
}

fn response_error(response: &Value) -> Option<String> {
    if response.get("ok").and_then(Value::as_bool) == Some(true) {
        None
    } else {
        Some(
            response
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("the device rejected the request")
                .to_string(),
        )
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct OpenTerminalArgs {
    /// Hint identifying which device to use when the user has more than one registered.
    pub device_hint: Option<String>,
}

#[derive(Clone)]
pub struct OpenTerminal {
    db: Option<Db>,
    user_id: UserId,
    hub: DeviceHub,
}

impl OpenTerminal {
    pub fn new(db: Option<Db>, user_id: UserId, hub: DeviceHub) -> Self {
        Self { db, user_id, hub }
    }
}

impl Tool for OpenTerminal {
    const NAME: &'static str = "open_terminal";
    type Args = OpenTerminalArgs;
    type Output = Value;
    type Error = TerminalToolError;

    fn description(&self) -> String {
        "Open a live interactive terminal (shell) session on one of the user's registered devices, such as their Mac. \
         Call this once before run_terminal_command if no session is open yet in this conversation."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "device_hint": {
                    "type": "string",
                    "description": "Optional hint identifying which device to use when the user has more than one registered (e.g. 'mac', 'macbook'). Omit if the user only has one device."
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(TerminalToolError::NotConfigured)?;
        let device = resolve_device(db, self.user_id.0, args.device_hint.as_deref()).await?;
        let link = device_link(&self.hub, &device)?;

        let response = link.request("open_shell", json!({}), OPEN_TIMEOUT).await?;
        if let Some(err) = response_error(&response) {
            return Err(TerminalToolError::DeviceUnavailable(err));
        }

        Ok(json!({
            "status": "opened",
            "device": device.label,
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RunTerminalCommandArgs {
    /// The exact shell command to run, e.g. "pmset -g batt".
    pub command: String,
    /// Hint identifying which device to use when the user has more than one registered.
    pub device_hint: Option<String>,
}

#[derive(Clone)]
pub struct RunTerminalCommand {
    db: Option<Db>,
    user_id: UserId,
    hub: DeviceHub,
    /// Identifies the agent turn this tool instance belongs to; a command is
    /// only run when it was proposed in an earlier turn (see `confirm_command`).
    #[allow(dead_code)]
    turn: Uuid,
}

impl RunTerminalCommand {
    pub fn new(db: Option<Db>, user_id: UserId, hub: DeviceHub, turn: Uuid) -> Self {
        Self {
            db,
            user_id,
            hub,
            turn,
        }
    }
}

async fn audit_device_command(db: &Db, user_id: Uuid, device_id: Uuid, details: Value) {
    let result = sqlx::query(
        "INSERT INTO audit_events (user_id, actor, event_type, affected_ids, details) \
         VALUES ($1, 'agent', 'device.command', $2, $3)",
    )
    .bind(user_id)
    .bind(json!([{ "type": "device", "id": device_id }]))
    .bind(details)
    .execute(db.pool())
    .await;
    if let Err(err) = result {
        tracing::error!(%err, %device_id, "failed to audit device command");
    }
}

impl Tool for RunTerminalCommand {
    const NAME: &'static str = "run_terminal_command";
    type Args = RunTerminalCommandArgs;
    type Output = Value;
    type Error = TerminalToolError;

    fn description(&self) -> String {
        "Run one shell command directly in the terminal session on one of the user's registered devices (such as their Mac), \
         and return its output. Directly executes the command and returns stdout/stderr without requiring a separate confirmation step. \
         The session keeps state (working directory, environment) between calls. Open a session first if none is open."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The exact shell command to run, e.g. 'pmset -g batt' or 'ls ~/Desktop'."
                },
                "device_hint": {
                    "type": "string",
                    "description": "Optional hint identifying which device to use when the user has more than one registered."
                }
            },
            "required": ["command"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(TerminalToolError::NotConfigured)?;
        let command = args.command.trim();
        if command.is_empty() {
            return Err(TerminalToolError::InvalidInput(
                "command cannot be empty".into(),
            ));
        }
        let device = resolve_device(db, self.user_id.0, args.device_hint.as_deref()).await?;
        let link = device_link(&self.hub, &device)?;

        let mut response = link
            .request(
                "run_command",
                json!({ "command": command }),
                COMMAND_TIMEOUT,
            )
            .await;

        // Auto-open terminal session if none was open yet
        if let Ok(ref res) = response
            && response_error(res).as_deref() == Some("no terminal session is open")
        {
            let _ = link.request("open_shell", json!({}), OPEN_TIMEOUT).await;
            response = link
                .request(
                    "run_command",
                    json!({ "command": command }),
                    COMMAND_TIMEOUT,
                )
                .await;
        }
        let response = match response {
            Ok(response) => response,
            Err(err) => {
                audit_device_command(
                    db,
                    self.user_id.0,
                    device.id,
                    json!({ "command": command, "outcome": "unreachable", "error": err.to_string() }),
                )
                .await;
                return Err(err.into());
            }
        };
        if let Some(err) = response_error(&response) {
            audit_device_command(
                db,
                self.user_id.0,
                device.id,
                json!({ "command": command, "outcome": "rejected", "error": err }),
            )
            .await;
            return Err(TerminalToolError::DeviceUnavailable(err));
        }

        let mut output = response
            .get("output")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        if output.chars().count() > MAX_OUTPUT_CHARS {
            output = output.chars().take(MAX_OUTPUT_CHARS).collect();
            output.push_str("\n… output truncated");
        }
        let exit_code = response.get("exit_code").and_then(Value::as_i64);
        audit_device_command(
            db,
            self.user_id.0,
            device.id,
            json!({ "command": command, "outcome": "executed", "exit_code": exit_code }),
        )
        .await;

        Ok(json!({
            "device": device.label,
            "output": output,
            "exit_code": exit_code,
        }))
    }
}
