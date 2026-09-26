use futures_util::StreamExt;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use uuid::Uuid;

use super::ConnectedAppError;
use crate::remote_extensions::adapters::transport::client_for_endpoint;

const PROTOCOL_VERSION: &str = "2025-06-18";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TOOL_PAGES: usize = 10;
/// Upper bound for any single request; callers pass tighter per-call limits.
const CLIENT_CEILING: Duration = Duration::from_secs(60);
/// Re-resolve and re-pin DNS this often, so an address change is picked up.
const CLIENT_TTL: Duration = Duration::from_secs(300);
/// Forget an idle session well before servers typically expire one.
const SESSION_IDLE_TTL: Duration = Duration::from_secs(600);

/// Negotiated state of one MCP session, reusable across tool calls.
#[derive(Clone, Debug)]
struct SessionState {
    session_id: Option<String>,
    protocol_version: String,
    server_info: Value,
    last_used: Instant,
}

/// Reuses DNS-pinned HTTP clients (and so their TLS / HTTP2 connections) per
/// endpoint, and MCP sessions per connected app and access token, so a warm
/// tool call is one round trip instead of initialize + initialized + call.
#[derive(Clone, Default)]
pub struct McpPool {
    clients: Arc<Mutex<HashMap<String, (reqwest::Client, Instant)>>>,
    sessions: Arc<Mutex<HashMap<(Uuid, String), SessionState>>>,
}

fn fingerprint(token: &str) -> String {
    hex::encode(&Sha256::digest(token.as_bytes())[..8])
}

impl McpPool {
    async fn client(
        &self,
        endpoint: &str,
        allow_local: bool,
    ) -> Result<reqwest::Client, ConnectedAppError> {
        if let Some((client, created)) = self.clients.lock().unwrap().get(endpoint)
            && created.elapsed() < CLIENT_TTL
        {
            return Ok(client.clone());
        }
        let client = client_for_endpoint(endpoint, CLIENT_CEILING, allow_local)
            .await
            .map_err(|e| ConnectedAppError::Provider(e.to_string()))?;
        self.clients
            .lock()
            .unwrap()
            .insert(endpoint.to_string(), (client.clone(), Instant::now()));
        Ok(client)
    }

    fn cached(&self, key: &(Uuid, String)) -> Option<SessionState> {
        let mut sessions = self.sessions.lock().unwrap();
        match sessions.get(key) {
            Some(state) if state.last_used.elapsed() < SESSION_IDLE_TTL => Some(state.clone()),
            Some(_) => {
                sessions.remove(key);
                None
            }
            None => None,
        }
    }

    fn store(&self, key: (Uuid, String), session: &McpSession) {
        self.sessions.lock().unwrap().insert(
            key,
            SessionState {
                session_id: session.session_id.clone(),
                protocol_version: session.protocol_version.clone(),
                server_info: session.server_info.clone(),
                last_used: Instant::now(),
            },
        );
    }

    /// Drop cached sessions for an app, e.g. after it was disconnected.
    pub fn forget(&self, extension_id: Uuid) {
        self.sessions
            .lock()
            .unwrap()
            .retain(|(id, _), _| *id != extension_id);
    }

    pub fn is_warm(&self, extension_id: Uuid, access_token: &str) -> bool {
        self.cached(&(extension_id, fingerprint(access_token)))
            .is_some()
    }

    async fn session(
        &self,
        extension_id: Uuid,
        endpoint: &str,
        access_token: &str,
        timeout: Duration,
        allow_local: bool,
    ) -> Result<(McpSession, bool), ConnectedAppError> {
        let http = self.client(endpoint, allow_local).await?;
        let key = (extension_id, fingerprint(access_token));
        if let Some(state) = self.cached(&key) {
            return Ok((
                McpSession {
                    http,
                    endpoint: endpoint.to_string(),
                    access_token: access_token.to_string(),
                    session_id: state.session_id,
                    protocol_version: state.protocol_version,
                    server_info: state.server_info,
                    timeout,
                },
                true,
            ));
        }
        let session = McpSession::open(http, endpoint, access_token, timeout).await?;
        self.store(key, &session);
        Ok((session, false))
    }

    /// Open a session ahead of the first call so it is warm when needed.
    pub async fn warm(
        &self,
        extension_id: Uuid,
        endpoint: &str,
        access_token: &str,
        timeout: Duration,
        allow_local: bool,
    ) -> Result<(), ConnectedAppError> {
        self.session(extension_id, endpoint, access_token, timeout, allow_local)
            .await
            .map(|_| ())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn call_tool(
        &self,
        extension_id: Uuid,
        endpoint: &str,
        access_token: &str,
        name: &str,
        arguments: Value,
        timeout: Duration,
        allow_local: bool,
    ) -> Result<Value, ConnectedAppError> {
        let key = (extension_id, fingerprint(access_token));
        let (mut session, reused) = self
            .session(extension_id, endpoint, access_token, timeout, allow_local)
            .await?;
        match session.call_tool(name, arguments.clone()).await {
            // The server forgot the session: start a new one and retry once.
            Err(ConnectedAppError::SessionExpired) if reused => {
                self.sessions.lock().unwrap().remove(&key);
                let http = self.client(endpoint, allow_local).await?;
                let mut fresh = McpSession::open(http, endpoint, access_token, timeout).await?;
                let result = fresh.call_tool(name, arguments).await;
                self.store(key, &fresh);
                result
            }
            Err(ConnectedAppError::Unauthorized) => {
                self.sessions.lock().unwrap().remove(&key);
                Err(ConnectedAppError::Unauthorized)
            }
            other => {
                self.store(key, &session);
                other
            }
        }
    }

    /// Open a fresh session and list the server's tools with its info.
    pub async fn discover(
        &self,
        extension_id: Uuid,
        endpoint: &str,
        access_token: &str,
        timeout: Duration,
        allow_local: bool,
    ) -> Result<(Value, Vec<Value>), ConnectedAppError> {
        let http = self.client(endpoint, allow_local).await?;
        let mut session = McpSession::open(http, endpoint, access_token, timeout).await?;
        let tools = session.list_tools().await?;
        self.store((extension_id, fingerprint(access_token)), &session);
        Ok((session.server_info.clone(), tools))
    }
}

/// One Streamable HTTP MCP session (spec 2025-06-18) authenticated with an
/// OAuth access token.
pub struct McpSession {
    http: reqwest::Client,
    endpoint: String,
    access_token: String,
    session_id: Option<String>,
    protocol_version: String,
    pub server_info: Value,
    timeout: Duration,
}

impl McpSession {
    async fn open(
        http: reqwest::Client,
        endpoint: &str,
        access_token: &str,
        timeout: Duration,
    ) -> Result<Self, ConnectedAppError> {
        let mut session = Self {
            http,
            endpoint: endpoint.to_string(),
            access_token: access_token.to_string(),
            session_id: None,
            protocol_version: PROTOCOL_VERSION.to_string(),
            server_info: Value::Null,
            timeout,
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
            .await
            .map_err(|e| match e {
                ConnectedAppError::SessionExpired => {
                    ConnectedAppError::Provider("the app refused to start a session".into())
                }
                other => other,
            })?;
        if let Some(version) = result.get("protocolVersion").and_then(Value::as_str) {
            session.protocol_version = version.to_string();
        }
        session.server_info = result.get("serverInfo").cloned().unwrap_or(Value::Null);
        session.notify("notifications/initialized").await?;
        Ok(session)
    }

    async fn list_tools(&mut self) -> Result<Vec<Value>, ConnectedAppError> {
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

    async fn call_tool(
        &mut self,
        name: &str,
        arguments: Value,
    ) -> Result<Value, ConnectedAppError> {
        self.request("tools/call", json!({"name": name, "arguments": arguments}))
            .await
    }

    fn post(&self, initialize: bool) -> reqwest::RequestBuilder {
        let mut builder = self
            .http
            .post(&self.endpoint)
            .timeout(self.timeout)
            .bearer_auth(&self.access_token)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        // The version header is only defined after negotiation.
        if !initialize {
            builder = builder.header("mcp-protocol-version", &self.protocol_version);
        }
        if let Some(id) = &self.session_id {
            builder = builder.header("mcp-session-id", id);
        }
        builder
    }

    async fn notify(&mut self, method: &str) -> Result<(), ConnectedAppError> {
        let response = self
            .post(false)
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
            .post(method == "initialize")
            .json(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .send()
            .await
            .map_err(network)?;
        let status = response.status();
        if status.as_u16() == 401 {
            return Err(ConnectedAppError::Unauthorized);
        }
        // Spec: a request for a session the server no longer knows gets 404.
        if status.as_u16() == 404 && self.session_id.is_some() {
            return Err(ConnectedAppError::SessionExpired);
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
            let text = String::from_utf8_lossy(&body).to_lowercase();
            if status.as_u16() == 400 && (text.contains("session") || text.contains("initializ")) {
                return Err(ConnectedAppError::SessionExpired);
            }
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
        ConnectedAppError::Timeout
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
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let text = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error");
        let lower = text.to_lowercase();
        if method == "tools/call"
            && (code == -32601
                || lower.contains("unknown tool")
                || lower.contains("tool not found"))
        {
            return Err(ConnectedAppError::UnknownTool);
        }
        if (lower.contains("session") && (lower.contains("not found") || lower.contains("expired")))
            || lower.contains("not initialized")
        {
            return Err(ConnectedAppError::SessionExpired);
        }
        return Err(ConnectedAppError::Provider(format!("{method}: {text}")));
    }
    message
        .get("result")
        .cloned()
        .ok_or_else(|| ConnectedAppError::Provider(format!("{method}: missing result")))
}
