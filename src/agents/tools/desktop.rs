use crate::{
    db::Db,
    desktop::service::{DesktopControlService, DesktopError},
    realtime::DeviceHub,
};
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopArgs {
    pub action: String,
    pub device_id: Option<Uuid>,
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub arguments: Value,
}
#[derive(Clone)]
pub struct ControlDesktopApp {
    pub db: Option<Db>,
    pub hub: Option<DeviceHub>,
    pub user_id: Uuid,
    pub session: Option<String>,
    pub turn: Option<String>,
}
impl Tool for ControlDesktopApp {
    const NAME: &'static str = "control_desktop_app";
    type Args = DesktopArgs;
    type Output = Value;
    type Error = DesktopError;
    fn description(&self) -> String {
        "Control the signed-in Vox desktop from any channel including phone. Use action=context first to inspect current page, open space, selection, timezone and revision. Use devices to resolve multiple desktops. Never guess IDs or claim completion without ok=true. Actions: navigate (page, spaceId or pulseId), timeline_filter (range=today/yesterday/this_week/last_week/custom, timezone, startDate/endDate inclusive for custom, merchant/category optional), select_item(itemId), terminal_panel(open boolean; opens panel only), create_space(intent,title), space_chat(message), add_node(kind,title,body), update_node(nodeId,title/body,expectedVersion), remove_node(nodeId,expectedVersion), connect_nodes(fromNodeId,toNodeId), arrange_canvas, stop_work, create_pulse(title,definition,idempotencyKey), update_pulse(pulseId,title,definition), list_pulse_measurements, list_spaces. Always pass spaceId from context in arguments for existing Space actions and pulseId for update_pulse. Pass expected_revision from context for selection-dependent actions; ask when references are ambiguous. Return real query results; spending is confirmed outflow, not authorization attempts.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","properties":{
        "action":{"type":"string","enum":["devices","context","navigate","timeline_filter","select_item","terminal_panel","create_space","space_chat","add_node","update_node","remove_node","connect_nodes","arrange_canvas","stop_work","create_pulse","update_pulse","list_pulse_measurements","list_spaces"]},
        "device_id":{"type":"string","format":"uuid"},"expected_revision":{"type":"integer","minimum":0},
        "arguments":{"type":"object","description":"Action-specific typed fields described in tool description. No arbitrary code or shell commands."}
    },"required":["action"],"additionalProperties":false})
    }
    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        args: DesktopArgs,
    ) -> Result<Value, DesktopError> {
        let service = DesktopControlService {
            pool: self
                .db
                .as_ref()
                .ok_or(DesktopError::Offline)?
                .pool()
                .clone(),
            hub: self.hub.clone().ok_or(DesktopError::Offline)?,
        };
        if args.action == "devices" {
            return Ok(json!({"ok":true,"devices":service.devices(self.user_id).await?}));
        }
        let id = service
            .target(self.user_id, args.device_id, self.session.as_deref())
            .await?;
        if args.action == "context" {
            return Ok(json!({"ok":true,"device_id":id,"context":service.context(id).await?}));
        }
        if ![
            "navigate",
            "timeline_filter",
            "select_item",
            "terminal_panel",
            "create_space",
            "space_chat",
            "add_node",
            "update_node",
            "remove_node",
            "connect_nodes",
            "arrange_canvas",
            "stop_work",
            "create_pulse",
            "update_pulse",
            "list_pulse_measurements",
            "list_spaces",
        ]
        .contains(&args.action.as_str())
        {
            return Err(DesktopError::Invalid);
        }
        if [
            "space_chat",
            "add_node",
            "update_node",
            "remove_node",
            "connect_nodes",
            "arrange_canvas",
            "stop_work",
            "select_item",
            "update_pulse",
        ]
        .contains(&args.action.as_str())
            && args.expected_revision.is_none()
        {
            return Ok(
                json!({"ok":false,"error":"Read context first and supply expected_revision for the selected target."}),
            );
        }
        if [
            "space_chat",
            "add_node",
            "update_node",
            "remove_node",
            "connect_nodes",
            "arrange_canvas",
            "stop_work",
        ]
        .contains(&args.action.as_str())
            && args.arguments["spaceId"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .is_none()
        {
            return Ok(
                json!({"ok":false,"error":"Read context and provide its spaceId explicitly in arguments."}),
            );
        }
        if args.action == "update_pulse"
            && args.arguments["pulseId"]
                .as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .is_none()
        {
            return Ok(
                json!({"ok":false,"error":"Read context and provide its pulseId explicitly in arguments."}),
            );
        }
        let command_id = if let (Some(session), Some(turn)) = (&self.session, &self.turn) {
            let payload = serde_json::to_vec(&json!([
                self.user_id,
                id,
                session,
                turn,
                args.action,
                args.arguments
            ]))
            .map_err(|_| DesktopError::Invalid)?;
            let hash = Sha256::digest(payload);
            let mut bytes = [0u8; 16];
            bytes.copy_from_slice(&hash[..16]);
            Uuid::from_bytes(bytes)
        } else {
            Uuid::new_v4()
        };
        let durable = [
            "create_space",
            "space_chat",
            "add_node",
            "update_node",
            "remove_node",
            "connect_nodes",
            "arrange_canvas",
            "stop_work",
        ]
        .contains(&args.action.as_str());
        if durable && let Some(result) = receipt(&service.pool, self.user_id, command_id).await? {
            return Ok(
                json!({"ok":true,"output":result,"reconciled":true,"ui_acknowledged":false}),
            );
        }
        match service
            .dispatch(
                id,
                &args.action,
                args.arguments,
                args.expected_revision,
                command_id,
            )
            .await
        {
            Ok(result) => {
                if durable
                    && result["ok"] != true
                    && let Some(receipt) = receipt(&service.pool, self.user_id, command_id).await?
                {
                    return Ok(
                        json!({"ok":true,"output":receipt,"reconciled":true,"ui_acknowledged":false}),
                    );
                }
                Ok(result)
            }
            Err(error) => {
                if durable
                    && let Some(result) = receipt(&service.pool, self.user_id, command_id).await?
                {
                    return Ok(
                        json!({"ok":true,"output":result,"reconciled":true,"ui_acknowledged":false}),
                    );
                }
                Err(error)
            }
        }
    }
}

async fn receipt(
    pool: &sqlx::PgPool,
    user: Uuid,
    command: Uuid,
) -> Result<Option<Value>, DesktopError> {
    Ok(sqlx::query_scalar::<_, Option<Value>>(
        "SELECT result FROM desktop_action_receipts WHERE user_id=$1 AND command_id=$2",
    )
    .bind(user)
    .bind(command)
    .fetch_optional(pool)
    .await?
    .flatten())
}
