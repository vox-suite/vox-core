use futures_util::StreamExt;
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

use super::ConnectedAppError;
use crate::remote_extensions::adapters::transport::client_for_endpoint;

const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_PAGES: usize = 10;

/// A Streamable HTTP MCP session (spec 2025-06-18) authenticated with an
/// OAuth access token. Every request goes through the DNS-pinned transport
/// that refuses private network targets.
pub struct McpSession {
    http: reqwest::Client,
    endpoint: String,
    access_token: String,
    session_id: Option<String>,
    protocol_version: String,
    pub server_info: Value,
}

impl McpSession {
    pub async fn open(
        endpoint: &str,
        access_token: &str,
        timeout: Duration,
        allow_local_for_testing: bool,
    ) -> Result<Self, ConnectedAppError> {
        let http = client_for_endpoint(endpoint, timeout, allow_local_for_testing)
            .await
            .map_err(|e| ConnectedAppError::Provider(e.to_string()))?;
        let mut session = Self {
            http,
            endpoint: endpoint.to_string(),
            access_token: access_token.to_string(),
            session_id: None,
            protocol_version: PROTOCOL_VERSION.to_string(),
            server_info: Value::Null,
        };
        let result = session
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": "Vox", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            session.protocol_version = version.to_string();
        }
        session.server_info = result.get("serverInfo").cloned().unwrap_or(Value::Null);
        session.notify("notifications/initialized").await?;
        Ok(session)
    }

    pub async fn list_tools(&mut self) -> Result<Vec<Value>, ConnectedAppError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_TOOL_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let result = self.request("tools/list", params).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            cursor = result
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(tools)
    }

    pub async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, ConnectedAppError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    fn post(&self) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.access_token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream")
            .header("mcp-protocol-version", &self.protocol_version);
        if let Some(id) = &self.session_id {
            builder = builder.header("mcp-session-id", id);
        }
        builder
    }

    async fn notify(&mut self, method: &str) -> Result<(), ConnectedAppError> {
        let response = self
            .post()
            .json(&json!({"jsonrpc": "2.0", "method": method}))
            .send()
            .await
            .map_err(network)?;
        if response.status().as_u16() == 401 {
            return Err(ConnectedAppError::Unauthorized);
        }
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, ConnectedAppError> {
        let id = Uuid::new_v4().to_string();
        let response = self
            .post()
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        if status.as_u16() == 401 {
            return Err(ConnectedAppError::Unauthorized);
        }
        if let Some(session_id) = response
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            self.session_id = Some(session_id.to_string());
        }
        let is_event_stream = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        let body = read_bounded(response).await?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&body);
            return Err(ConnectedAppError::Provider(format!(
                "{method} returned HTTP {}: {}",
                status.as_u16(),
                text.chars().take(300).collect::<String>()
            )));
        }
        let message = if is_event_stream {
            message_from_event_stream(&body, &id)?
        } else {
            serde_json::from_slice::<Value>(&body)
                .map_err(|_| ConnectedAppError::Provider(format!("{method}: invalid JSON")))?
        };
        rpc_result(message, &id, method)
    }
}

fn network(error: reqwest::Error) -> ConnectedAppError {
    if error.is_timeout() {
        ConnectedAppError::Provider("the app did not respond in time".into())
    } else {
        ConnectedAppError::Provider("could not reach the app".into())
    }
}

async fn read_bounded(response: reqwest::Response) -> Result<Vec<u8>, ConnectedAppError> {
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network)?;
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(ConnectedAppError::Provider(
                "app response is too large".into(),
            ));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// Pick the JSON-RPC response for `id` out of an SSE body. Servers may send
/// notifications or progress events on the same stream before the response.
fn message_from_event_stream(body: &[u8], id: &str) -> Result<Value, ConnectedAppError> {
    let text = String::from_utf8_lossy(body);
    for event in text.split("\n\n") {
        let data: String = event
            .lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .map(str::trim_start)
            .collect::<Vec<_>>()
            .join("\n");
        if data.is_empty() {
            continue;
        }
        if let Ok(message) = serde_json::from_str::<Value>(&data)
            && message.get("id").and_then(Value::as_str) == Some(id)
        {
            return Ok(message);
        }
    }
    Err(ConnectedAppError::Provider(
        "app stream ended without a response".into(),
    ))
}

fn rpc_result(message: Value, id: &str, method: &str) -> Result<Value, ConnectedAppError> {
    if message.get("id").and_then(Value::as_str) != Some(id) {
        return Err(ConnectedAppError::Provider(format!(
            "{method}: response id mismatch"
        )));
    }
    if let Some(error) = message.get("error") {
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        return Err(ConnectedAppError::Provider(format!("{method}: {text}")));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| ConnectedAppError::Provider(format!("{method}: missing result")))
}
