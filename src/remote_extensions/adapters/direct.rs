use super::transport::client_for_endpoint;
use super::{
    AdapterExecutionError, ExtensionInvocation, ExtensionProtocolAdapter, ExtensionReconciliation,
    NormalizedResponse, ResponseStatus, integrity::ExtensionIntegritySigner,
};
use crate::remote_extensions::{AuthorizedEndpoint, ExtensionProtocol};
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;
use uuid::Uuid;

pub const MAX_PAYLOAD_BYTES: usize = 2 * 1024 * 1024; // 2 MB limit
pub const DEFAULT_TIMEOUT_SECONDS: u64 = 10;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DirectApiRequest {
    pub action: String,
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<Uuid>,
}

pub struct DirectProtocolAdapter {
    timeout: Duration,
    allow_local_for_testing: bool,
}

impl DirectProtocolAdapter {
    pub fn new() -> Self {
        Self::with_timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECONDS))
    }

    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            allow_local_for_testing: false,
        }
    }

    /// Isolated loopback mock servers in integration tests only.
    pub fn with_local_endpoints_for_testing(mut self) -> Self {
        self.allow_local_for_testing = true;
        self
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Default for DirectProtocolAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ExtensionProtocolAdapter for DirectProtocolAdapter {
    fn protocol(&self) -> ExtensionProtocol {
        ExtensionProtocol::Direct
    }

    async fn execute(
        &self,
        endpoint: &AuthorizedEndpoint,
        invocation: &ExtensionInvocation,
        secret: Option<&[u8]>,
    ) -> Result<NormalizedResponse, AdapterExecutionError> {
        let request_payload = DirectApiRequest {
            action: invocation.capability_key.clone(),
            parameters: invocation.parameters.clone(),
            idempotency_key: invocation.idempotency_key.clone(),
            execution_id: invocation.execution_id,
        };

        let body_bytes = serde_json::to_vec(&request_payload)
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

        let client = client_for_endpoint(
            &endpoint.endpoint_url,
            self.timeout,
            self.allow_local_for_testing,
        )
        .await?;
        let mut req_builder = client
            .post(&endpoint.endpoint_url)
            .header("content-type", "application/json")
            .header("x-vox-protocol", "direct/v1");

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
        let mut resp_bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|e| AdapterExecutionError::Network(e.to_string()))?;
            if resp_bytes.len().saturating_add(chunk.len()) > MAX_PAYLOAD_BYTES {
                return Err(AdapterExecutionError::InvalidPayload(
                    "direct response exceeds the 2 MiB limit".into(),
                ));
            }
            resp_bytes.extend_from_slice(&chunk);
        }

        let status_code = status.as_u16();

        let parsed_json: Option<Value> = serde_json::from_slice(&resp_bytes).ok();

        if !status.is_success() {
            let err_body = String::from_utf8_lossy(&resp_bytes);
            // Provider-supplied error codes are untrusted strings and may carry
            // credentials. Preserve only the HTTP status as a stable safe code.
            let code = format!("HTTP_{status_code}");

            let message = parsed_json
                .as_ref()
                .and_then(|j| j.get("error_message").or_else(|| j.get("message")))
                .and_then(Value::as_str)
                .map(String::from)
                .unwrap_or_else(|| err_body.into_owned());

            return Ok(NormalizedResponse {
                status: if status.is_client_error() {
                    ResponseStatus::ClientError
                } else {
                    ResponseStatus::ProviderError
                },
                data: parsed_json.unwrap_or(Value::Null),
                provider_reference: None,
                guarantees_reported: None,
                error_code: Some(code),
                error_message: Some(message),
            });
        }

        let data = parsed_json.unwrap_or_else(|| {
            json!({
                "raw_text": String::from_utf8_lossy(&resp_bytes)
            })
        });

        let provider_ref = data
            .get("provider_reference")
            .or_else(|| data.get("reference"))
            .or_else(|| data.get("id"))
            .and_then(Value::as_str)
            .map(String::from);

        let guarantees_reported = data.get("guarantees").cloned();

        Ok(NormalizedResponse {
            status: ResponseStatus::Success,
            data,
            provider_reference: provider_ref,
            guarantees_reported,
            error_code: None,
            error_message: None,
        })
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
                error_message: Some("Direct reconciliation request timed out".into()),
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
