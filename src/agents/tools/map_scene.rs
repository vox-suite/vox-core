use crate::{map_scene::MapScene, realtime::UserEventHub};
use rig::tool::Tool;
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum MapToolError {
    #[error("Invalid scene: {0}")]
    Invalid(String),
    #[error("Map is not available")]
    NotConfigured,
}

#[derive(Clone)]
pub struct ShowOnMap {
    hub: Option<UserEventHub>,
    user_id: Uuid,
}

impl ShowOnMap {
    pub fn new(hub: Option<UserEventHub>, user_id: Uuid) -> Self {
        Self { hub, user_id }
    }
}

impl Tool for ShowOnMap {
    const NAME: &'static str = "show_on_map";
    type Args = MapScene;
    type Output = Value;
    type Error = MapToolError;

    fn description(&self) -> String {
        "Show things on the user's 3D map while talking: fly the camera, drop pins, draw animated arcs between places, grow spend columns, highlight buildings. Replaces whatever is on the map. Coordinates are longitude/latitude. Pin kind is place, task or spend. Arcs animate in order using delayMs. Use only coordinates you got from a tool result.".to_owned()
    }

    fn parameters(&self) -> Value {
        let point = json!({
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "lng": { "type": "number" },
                "lat": { "type": "number" }
            },
            "required": ["id", "lng", "lat"]
        });
        json!({
            "type": "object",
            "properties": {
                "camera": {
                    "type": "object",
                    "properties": {
                        "lng": { "type": "number" },
                        "lat": { "type": "number" },
                        "zoom": { "type": "number", "description": "10 to 19; 16 is street level" },
                        "pitch": { "type": "number" },
                        "bearing": { "type": "number" },
                        "durationMs": { "type": "integer" }
                    },
                    "required": ["lng", "lat"]
                },
                "pins": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "lng": { "type": "number" },
                            "lat": { "type": "number" },
                            "label": { "type": "string" },
                            "kind": { "type": "string", "enum": ["place", "task", "spend"] },
                            "spanId": { "type": "string", "description": "Task id, to keep this pin in sync with the task's status" }
                        },
                        "required": ["id", "lng", "lat", "kind"]
                    }
                },
                "arcs": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "from": { "type": "array", "items": { "type": "number" }, "description": "[lng, lat]" },
                            "to": { "type": "array", "items": { "type": "number" }, "description": "[lng, lat]" },
                            "label": { "type": "string" },
                            "delayMs": { "type": "integer" }
                        },
                        "required": ["id", "from", "to"]
                    }
                },
                "columns": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "lng": { "type": "number" },
                            "lat": { "type": "number" },
                            "value": { "type": "number", "description": "Relative height, for example amount spent" },
                            "label": { "type": "string" }
                        },
                        "required": ["id", "lng", "lat", "value"]
                    }
                },
                "highlights": { "type": "array", "items": point },
                "narrationHint": { "type": "string" }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let hub = self.hub.as_ref().ok_or(MapToolError::NotConfigured)?;
        args.validate().map_err(MapToolError::Invalid)?;
        let scene = hub.set_scene(self.user_id, args);
        Ok(json!({ "shown": true, "rev": scene.rev }))
    }
}

#[derive(Clone)]
pub struct ClearMap {
    hub: Option<UserEventHub>,
    user_id: Uuid,
}

impl ClearMap {
    pub fn new(hub: Option<UserEventHub>, user_id: Uuid) -> Self {
        Self { hub, user_id }
    }
}

impl Tool for ClearMap {
    const NAME: &'static str = "clear_map";
    type Args = Value;
    type Output = Value;
    type Error = MapToolError;

    fn description(&self) -> String {
        "Clear everything shown on the user's map and return the camera to their home view."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let hub = self.hub.as_ref().ok_or(MapToolError::NotConfigured)?;
        hub.set_scene(self.user_id, MapScene::default());
        Ok(json!({ "cleared": true }))
    }
}
