use rig::tool::{DynamicTool, ToolExecutionError, ToolOutput};
use serde_json::{Map, Value, json};
use std::{collections::HashSet, sync::Arc};
use uuid::Uuid;

use super::{ConnectedAppError, ConnectedAppsService, ConnectedTool};
use crate::identity::UserId;

/// Upper bound on connected-app tools offered in one turn, so a user with
/// many apps does not exceed the model's function limit.
const MAX_TOOLS: usize = 96;
const MAX_RESULT_CHARS: usize = 16_000;
const MAX_NAME_LEN: usize = 64;

/// The user's connected-app tools as agent tools for this turn.
pub async fn agent_tools(
    service: &Arc<ConnectedAppsService>,
    user_id: UserId,
    turn: Uuid,
) -> Vec<DynamicTool> {
    let tools = match service.tools_for_user(user_id).await {
        Ok(tools) => tools,
        Err(err) => {
            tracing::warn!(%err, "connected app tools unavailable");
            return Vec::new();
        }
    };
    let mut used = HashSet::new();
    tools
        .into_iter()
        .filter_map(|tool| build(service.clone(), user_id, turn, tool, &mut used))
        .take(MAX_TOOLS)
        .collect()
}

fn build(
    service: Arc<ConnectedAppsService>,
    user_id: UserId,
    turn: Uuid,
    connected: ConnectedTool,
    used: &mut HashSet<String>,
) -> Option<DynamicTool> {
    let mcp_name = connected.tool.get("name")?.as_str()?.to_string();
    let name = unique_name(&connected.app_key, &mcp_name, used);
    let read_only = connected
        .tool
        .pointer("/annotations/readOnlyHint")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let summary = connected
        .tool
        .get("description")
        .and_then(Value::as_str)
        .or_else(|| {
            connected
                .tool
                .pointer("/annotations/title")
                .and_then(Value::as_str)
        })
        .unwrap_or("")
        .chars()
        .take(900)
        .collect::<String>();
    let app_name = connected.app_name.clone();
    let description = if read_only {
        format!("[{app_name}] {summary}")
    } else {
        format!(
            "[{app_name}] {summary} This acts on the user's {app_name} account, so it only runs after the user confirms."
        )
    };
    let parameters = sanitize_schema(
        connected
            .tool
            .get("inputSchema")
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
    );
    let extension_id = connected.extension_id;
    let tool_name = name.clone();
    Some(DynamicTool::new(
        name,
        description,
        parameters,
        move |_context, arguments| {
            let service = service.clone();
            let mcp_name = mcp_name.clone();
            let app_name = app_name.clone();
            let tool_name = tool_name.clone();
            Box::pin(async move {
                if !read_only {
                    match service
                        .confirm_or_propose(user_id, extension_id, &mcp_name, &arguments, turn)
                        .await
                    {
                        Ok(true) => {}
                        Ok(false) => {
                            return Ok(ToolOutput::json(json!({
                                "status": "needs_confirmation",
                                "instruction": format!(
                                    "Nothing has happened yet. Tell the user exactly what this will do in {app_name} and ask them to confirm. Only if they confirm in their next message, call {tool_name} again with exactly the same arguments."
                                ),
                            })));
                        }
                        Err(err) => return Err(tool_error(err)),
                    }
                }
                let result = service
                    .call_tool(user_id, extension_id, &mcp_name, arguments)
                    .await
                    .map_err(tool_error)?;
                present(result)
            })
        },
    ))
}

fn tool_error(err: ConnectedAppError) -> ToolExecutionError {
    match err {
        ConnectedAppError::Unauthorized => ToolExecutionError::permission_denied(
            "The app connection expired. Ask the user to reconnect it on the Apps page.",
        ),
        ConnectedAppError::NotFound => {
            ToolExecutionError::not_found("This app is no longer connected.")
        }
        other => ToolExecutionError::provider(other.to_string()),
    }
}

/// Turn an MCP `tools/call` result into compact model input.
fn present(result: Value) -> Result<ToolOutput, ToolExecutionError> {
    let text: String = result
        .get("content")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| match item.get("type").and_then(Value::as_str) {
                    Some("text") => item.get("text").and_then(Value::as_str).map(str::to_string),
                    Some("resource") => item
                        .pointer("/resource/text")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    Some("resource_link") => item
                        .get("uri")
                        .and_then(Value::as_str)
                        .map(|uri| format!("Link: {uri}")),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(ToolExecutionError::provider(truncate(if text.is_empty() {
            "The app reported an error.".into()
        } else {
            text
        })));
    }
    if let Some(structured) = result.get("structuredContent")
        && !structured.is_null()
    {
        let rendered = structured.to_string();
        if rendered.chars().count() <= MAX_RESULT_CHARS {
            return Ok(ToolOutput::json(structured.clone()));
        }
    }
    Ok(ToolOutput::text(truncate(if text.is_empty() {
        "Done.".into()
    } else {
        text
    })))
}

fn truncate(text: String) -> String {
    if text.chars().count() <= MAX_RESULT_CHARS {
        text
    } else {
        let mut cut: String = text.chars().take(MAX_RESULT_CHARS).collect();
        cut.push_str("\n…(truncated)");
        cut
    }
}

/// Function names must start with a letter and use `[A-Za-z0-9_]`, max 64.
fn unique_name(app_key: &str, tool: &str, used: &mut HashSet<String>) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect()
    };
    let mut base = format!("{}__{}", clean(app_key), clean(tool));
    if !base.starts_with(|c: char| c.is_ascii_alphabetic()) {
        base.insert(0, 'a');
    }
    base.truncate(MAX_NAME_LEN);
    let mut name = base.clone();
    let mut n = 2;
    while !used.insert(name.clone()) {
        let suffix = format!("_{n}");
        let mut trimmed = base.clone();
        trimmed.truncate(MAX_NAME_LEN - suffix.len());
        name = format!("{trimmed}{suffix}");
        n += 1;
    }
    name
}

/// Reduce an MCP input schema to the subset every model provider accepts:
/// type, description, properties, items, required and string enums. Unions
/// collapse to their first non-null member.
pub fn sanitize_schema(schema: Value) -> Value {
    let mut root = sanitize(&schema, 0);
    if root.get("type").and_then(Value::as_str) != Some("object") {
        root = json!({"type": "object", "properties": {}});
    }
    if root.get("properties").is_none() {
        root["properties"] = json!({});
    }
    root
}

fn sanitize(schema: &Value, depth: usize) -> Value {
    let Some(obj) = schema.as_object() else {
        return json!({"type": "string"});
    };
    if depth > 8 {
        return json!({"type": "string"});
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(first) = obj.get(key).and_then(Value::as_array).and_then(|options| {
            options
                .iter()
                .find(|o| o.get("type").and_then(Value::as_str) != Some("null"))
        }) {
            let mut merged = sanitize(first, depth + 1);
            if let Some(desc) = obj.get("description") {
                merged["description"] = desc.clone();
            }
            return merged;
        }
    }
    let ty = match obj.get("type") {
        Some(Value::String(t)) => t.clone(),
        Some(Value::Array(types)) => types
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("string")
            .to_string(),
        _ if obj.contains_key("properties") => "object".into(),
        _ if obj.contains_key("items") => "array".into(),
        _ => "string".into(),
    };
    let mut out = Map::new();
    out.insert("type".into(), Value::String(ty.clone()));
    if let Some(desc) = obj.get("description").and_then(Value::as_str) {
        out.insert(
            "description".into(),
            Value::String(desc.chars().take(500).collect()),
        );
    }
    match ty.as_str() {
        "object" => {
            let mut properties = Map::new();
            if let Some(props) = obj.get("properties").and_then(Value::as_object) {
                for (key, value) in props {
                    properties.insert(key.clone(), sanitize(value, depth + 1));
                }
            }
            let required: Vec<Value> = obj
                .get("required")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter(|k| k.as_str().is_some_and(|k| properties.contains_key(k)))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            out.insert("properties".into(), Value::Object(properties));
            if !required.is_empty() {
                out.insert("required".into(), Value::Array(required));
            }
        }
        "array" => {
            let items = obj
                .get("items")
                .map(|i| sanitize(i, depth + 1))
                .unwrap_or_else(|| json!({"type": "string"}));
            out.insert("items".into(), items);
        }
        "string" => {
            if let Some(values) = obj.get("enum").and_then(Value::as_array) {
                let strings: Vec<Value> =
                    values.iter().filter(|v| v.is_string()).cloned().collect();
                if !strings.is_empty() {
                    out.insert("enum".into(), Value::Array(strings));
                }
            }
        }
        _ => {}
    }
    Value::Object(out)
}
