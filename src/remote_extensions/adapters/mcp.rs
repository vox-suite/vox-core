use super::{
    AdapterExecutionError, ExtensionInvocation, ExtensionProtocolAdapter, ExtensionReconciliation,
    NormalizedResponse, ResponseStatus, integrity::ExtensionIntegritySigner,
};
use crate::remote_extensions::{AuthorizedEndpoint, ExtensionProtocol};
use chrono::Utc;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024; // 2 MB limit
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,
    pub id: Value,
    pub method: String,
    pub params: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

pub struct McpProtocolAdapter {
    client: Client,
    timeout: Duration,
}

impl McpProtocolAdapter {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            client: Client::builder()
                .timeout(timeout)
                .build()
                .unwrap_or_default(),
            timeout,
        }
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Default for McpProtocolAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ExtensionProtocolAdapter for McpProtocolAdapter {
    fn protocol(&self) -> ExtensionProtocol {
        ExtensionProtocol::Mcp
    }

    async fn execute(
        &self,
        endpoint: &AuthorizedEndpoint,
        invocation: &ExtensionInvocation,
        secret: Option<&[u8]>,
    ) -> Result<NormalizedResponse, AdapterExecutionError> {
        let req_id = Value::String(Uuid::new_v4().to_string());
        let rpc_request = JsonRpcRequest {
            jsonrpc: "2.0".into(),
            id: req_id,
            method: "tools/call".into(),
            params: json!({
                "name": invocation.capability_key,
                "arguments": invocation.parameters,
            }),
        };

        let body_bytes = serde_json::to_vec(&rpc_request)
            .map_err(|e| AdapterExecutionError::InvalidPayload(e.to_string()))?;

        if body_bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(AdapterExecutionError::InvalidPayload(format!(
                "payload size {} bytes exceeds maximum allowed {}",
                body_bytes.len(),
                MAX_PAYLOAD_BYTES
            )));
        }

        let now = Utc::now();
        let nonce = Uuid::new_v4();

        let mut req_builder = self
            .client
            .post(&endpoint.endpoint_url)
            .header("content-type", "application/json")
            .header("x-vox-protocol", "mcp/2024-11-05");

        if let Some(ref ikey) = invocation.idempotency_key {
            req_builder = req_builder.header("x-vox-idempotency-key", ikey);
        }

        if let Some(sec) = secret {
            let assertion = ExtensionIntegritySigner::sign(
                sec,
                endpoint.extension_id,
                &invocation.capability_key,
                &body_bytes,
                now,
                nonce,
            )?;
            req_builder = req_builder
                .header("x-vox-extension-assertion", assertion.signature)
                .header(
                    "x-vox-extension-timestamp",
                    assertion.timestamp.timestamp().to_string(),
                )
                .header("x-vox-extension-nonce", assertion.nonce.to_string())
                .header("x-vox-payload-hash", assertion.payload_hash);
        }

        let response = req_builder.body(body_bytes).send().await.map_err(|e| {
            if e.is_timeout() {
                AdapterExecutionError::Timeout(e.to_string())
            } else {
                AdapterExecutionError::Network(e.to_string())
            }
        })?;

        let status = response.status();
        let resp_bytes = response
            .bytes()
            .await
            .map_err(|e| AdapterExecutionError::Network(e.to_string()))?;

        if resp_bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(AdapterExecutionError::InvalidPayload(format!(
                "response size {} bytes exceeds maximum allowed {}",
                resp_bytes.len(),
                MAX_PAYLOAD_BYTES
            )));
        }

        if !status.is_success() {
            let status_code = status.as_u16();
            let err_body = String::from_utf8_lossy(&resp_bytes);
            return Ok(NormalizedResponse {
                status: if status.is_client_error() {
                    ResponseStatus::ClientError
                } else {
                    ResponseStatus::ProviderError
                },
                data: Value::Null,
                provider_reference: None,
                guarantees_reported: None,
                error_code: Some(format!("HTTP_{status_code}")),
                error_message: Some(err_body.into_owned()),
            });
        }

        let rpc_response: JsonRpcResponse = match serde_json::from_slice(&resp_bytes) {
            Ok(parsed) => parsed,
            Err(e) => {
                // Defensive fallback: if payload is plain JSON or not strict JSON-RPC
                if let Ok(raw_json) = serde_json::from_slice::<Value>(&resp_bytes) {
                    return Ok(NormalizedResponse {
                        status: ResponseStatus::Success,
                        data: raw_json,
                        provider_reference: None,
                        guarantees_reported: None,
                        error_code: None,
                        error_message: None,
                    });
                }
                return Err(AdapterExecutionError::ProtocolError(format!(
                    "failed to parse JSON-RPC response: {e}"
                )));
            }
        };

        if let Some(err) = rpc_response.error {
            return Ok(NormalizedResponse {
                status: ResponseStatus::ProviderError,
                data: err.data.unwrap_or(Value::Null),
                provider_reference: None,
                guarantees_reported: None,
                error_code: Some(err.code.to_string()),
                error_message: Some(err.message),
            });
        }

        let result = rpc_response.result.unwrap_or(Value::Null);

        // Parse MCP tool result convention:
        // { "content": [ { "type": "text", "text": "..." } ], "isError": false, "_meta": { ... } }
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let provider_ref = result
            .get("_meta")
            .and_then(|m| m.get("provider_reference"))
            .and_then(Value::as_str)
            .map(String::from)
            .or_else(|| {
                result
                    .get("provider_reference")
                    .and_then(Value::as_str)
                    .map(String::from)
            });

        let guarantees_reported = result
            .get("_meta")
            .and_then(|m| m.get("guarantees"))
            .cloned()
            .or_else(|| result.get("guarantees").cloned());

        let parsed_data =
            if let Some(content_array) = result.get("content").and_then(Value::as_array) {
                if let Some(first_text) = content_array
                    .iter()
                    .find(|c| c.get("type").and_then(Value::as_str) == Some("text"))
                    .and_then(|c| c.get("text").and_then(Value::as_str))
                {
                    // Attempt to parse text content as JSON if available
                    serde_json::from_str::<Value>(first_text)
                        .unwrap_or_else(|_| json!({ "text": first_text }))
                } else {
                    result.clone()
                }
            } else {
                result.clone()
            };

        if is_error {
            Ok(NormalizedResponse {
                status: ResponseStatus::ProviderError,
                data: parsed_data.clone(),
                provider_reference: provider_ref,
                guarantees_reported,
                error_code: Some("MCP_TOOL_ERROR".into()),
                error_message: parsed_data
                    .get("text")
                    .and_then(Value::as_str)
                    .map(String::from),
            })
        } else {
            Ok(NormalizedResponse {
                status: ResponseStatus::Success,
                data: parsed_data,
                provider_reference: provider_ref,
                guarantees_reported,
                error_code: None,
                error_message: None,
            })
        }
    }

    async fn reconcile(
        &self,
        endpoint: &AuthorizedEndpoint,
        reconciliation: &ExtensionReconciliation,
        secret: Option<&[u8]>,
    ) -> Result<NormalizedResponse, AdapterExecutionError> {
        let invocation = ExtensionInvocation {
            capability_key: format!("{}.reconcile", reconciliation.capability_key),
            parameters: json!({
                "execution_id": reconciliation.execution_id,
                "idempotency_key": reconciliation.idempotency_key,
                "provider_reference": reconciliation.provider_reference,
            }),
            access_context: Value::Null,
            idempotency_key: Some(reconciliation.idempotency_key.clone()),
            execution_id: Some(reconciliation.execution_id),
            required_guarantees: vec![],
        };

        match self.execute(endpoint, &invocation, secret).await {
            Ok(res) => Ok(res),
            Err(AdapterExecutionError::Timeout(_)) => Ok(NormalizedResponse {
                status: ResponseStatus::Timeout,
                data: Value::Null,
                provider_reference: reconciliation.provider_reference.clone(),
                guarantees_reported: None,
                error_code: Some("RECONCILE_TIMEOUT".into()),
                error_message: Some("Reconciliation remote request timed out".into()),
            }),
            Err(e) => Ok(NormalizedResponse {
                status: ResponseStatus::Uncertain,
                data: Value::Null,
                provider_reference: reconciliation.provider_reference.clone(),
                guarantees_reported: None,
                error_code: Some("RECONCILE_ERROR".into()),
                error_message: Some(e.to_string()),
            }),
        }
    }
}
