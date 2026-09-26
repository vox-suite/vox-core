use super::{AuthorizedEndpoint, ExtensionProtocol};
use crate::privacy::PrivacyService;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use uuid::Uuid;

pub mod direct;
pub mod execution;
pub mod integrity;
pub mod mcp;
mod transport;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseStatus {
    Success,
    ClientError,
    ProviderError,
    Timeout,
    Uncertain,
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterExecutionError {
    #[error("invalid payload: {0}")]
    InvalidPayload(String),
    #[error("network transport error: {0}")]
    Network(String),
    #[error("remote call timed out: {0}")]
    Timeout(String),
    #[error("provider rejected with code {code}: {message}")]
    ProviderRejected { code: String, message: String },
    #[error("protocol error: {0}")]
    ProtocolError(String),
    #[error("unsupported provider guarantee: {0}")]
    UnsupportedGuarantee(String),
    #[error("protocol adapter {0:?} is disabled")]
    AdapterDisabled(ExtensionProtocol),
    #[error("cryptographic integrity check failed: {0}")]
    IntegrityError(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtensionInvocation {
    pub capability_key: String,
    pub parameters: Value,
    #[serde(default)]
    pub access_context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<Uuid>,
    #[serde(default)]
    pub required_guarantees: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct NormalizedResponse {
    pub status: ResponseStatus,
    pub data: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guarantees_reported: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ExtensionReconciliation {
    pub capability_key: String,
    pub execution_id: Uuid,
    pub idempotency_key: String,
    pub provider_reference: Option<String>,
}

#[async_trait::async_trait]
pub trait ExtensionProtocolAdapter: Send + Sync {
    fn protocol(&self) -> ExtensionProtocol;

    async fn execute(
        &self,
        endpoint: &AuthorizedEndpoint,
        invocation: &ExtensionInvocation,
        secret: Option<&[u8]>,
    ) -> Result<NormalizedResponse, AdapterExecutionError>;

    async fn reconcile(
        &self,
        endpoint: &AuthorizedEndpoint,
        reconciliation: &ExtensionReconciliation,
        secret: Option<&[u8]>,
    ) -> Result<NormalizedResponse, AdapterExecutionError>;
}

/// Router and mediator for protocol adapters with dynamic enable/disable and context minimization.
pub struct ProtocolRouter {
    mcp_adapter: Arc<dyn ExtensionProtocolAdapter>,
    direct_adapter: Arc<dyn ExtensionProtocolAdapter>,
    mcp_enabled: AtomicBool,
    direct_enabled: AtomicBool,
    signing_secret: Arc<Vec<u8>>,
}

impl ProtocolRouter {
    pub fn new(
        mcp: Arc<dyn ExtensionProtocolAdapter>,
        direct: Arc<dyn ExtensionProtocolAdapter>,
        signing_secret: Vec<u8>,
    ) -> Self {
        Self {
            mcp_adapter: mcp,
            direct_adapter: direct,
            mcp_enabled: AtomicBool::new(true),
            direct_enabled: AtomicBool::new(true),
            signing_secret: Arc::new(signing_secret),
        }
    }

    pub fn set_adapter_enabled(&self, protocol: ExtensionProtocol, enabled: bool) {
        match protocol {
            ExtensionProtocol::Mcp => self.mcp_enabled.store(enabled, Ordering::SeqCst),
            ExtensionProtocol::Direct => self.direct_enabled.store(enabled, Ordering::SeqCst),
        }
    }

    pub fn is_adapter_enabled(&self, protocol: ExtensionProtocol) -> bool {
        match protocol {
            ExtensionProtocol::Mcp => self.mcp_enabled.load(Ordering::SeqCst),
            ExtensionProtocol::Direct => self.direct_enabled.load(Ordering::SeqCst),
        }
    }

    pub async fn execute(
        &self,
        endpoint: &AuthorizedEndpoint,
        invocation: &ExtensionInvocation,
    ) -> Result<NormalizedResponse, AdapterExecutionError> {
        if !self.is_adapter_enabled(endpoint.protocol) {
            return Err(AdapterExecutionError::AdapterDisabled(endpoint.protocol));
        }

        // Validate required guarantees against capability's optional guarantees or manifest
        Self::validate_guarantees(
            &invocation.required_guarantees,
            &endpoint.capability.optional_guarantees(),
        )?;

        // Minimize context: filter parameters to declared access needs
        let minimized_parameters =
            Self::minimize_context(&invocation.parameters, &endpoint.capability.access_needs);

        let minimized_invocation = ExtensionInvocation {
            capability_key: invocation.capability_key.clone(),
            parameters: minimized_parameters,
            access_context: Value::Null, // internal context strictly stripped
            idempotency_key: invocation.idempotency_key.clone(),
            execution_id: invocation.execution_id,
            required_guarantees: invocation.required_guarantees.clone(),
        };

        let secret = self.signing_secret.as_slice();
        let raw_response = match endpoint.protocol {
            ExtensionProtocol::Mcp => {
                self.mcp_adapter
                    .execute(endpoint, &minimized_invocation, Some(secret))
                    .await?
            }
            ExtensionProtocol::Direct => {
                self.direct_adapter
                    .execute(endpoint, &minimized_invocation, Some(secret))
                    .await?
            }
        };

        Ok(Self::redact_response(raw_response))
    }

    pub async fn reconcile(
        &self,
        endpoint: &AuthorizedEndpoint,
        reconciliation: &ExtensionReconciliation,
    ) -> Result<NormalizedResponse, AdapterExecutionError> {
        if !self.is_adapter_enabled(endpoint.protocol) {
            return Err(AdapterExecutionError::AdapterDisabled(endpoint.protocol));
        }

        let secret = self.signing_secret.as_slice();
        let raw_response = match endpoint.protocol {
            ExtensionProtocol::Mcp => {
                self.mcp_adapter
                    .reconcile(endpoint, reconciliation, Some(secret))
                    .await?
            }
            ExtensionProtocol::Direct => {
                self.direct_adapter
                    .reconcile(endpoint, reconciliation, Some(secret))
                    .await?
            }
        };

        Ok(Self::redact_response(raw_response))
    }

    /// Minimizes context sent to remote extensions to strictly adhere to declared access_needs.
    pub fn minimize_context(parameters: &Value, access_needs: &[String]) -> Value {
        // Forbidden sensitive keys that must NEVER be passed to external extensions
        const BLOCKED_INTERNAL_KEYS: &[&str] = &[
            "system_prompt",
            "internal_token",
            "session_token",
            "vox_auth",
            "secret_key",
            "database_url",
            "host_app_credentials",
            "raw_user_id",
            "credential",
            "password",
            "token",
            "secret",
            "api_key",
            "cookie",
        ];

        fn strip_internal(value: &Value) -> Value {
            match value {
                Value::Object(map) => Value::Object(
                    map.iter()
                        .filter_map(|(key, value)| {
                            let lower = key.to_ascii_lowercase();
                            if BLOCKED_INTERNAL_KEYS
                                .iter()
                                .any(|blocked| lower.contains(blocked))
                            {
                                None
                            } else {
                                Some((key.clone(), strip_internal(value)))
                            }
                        })
                        .collect(),
                ),
                Value::Array(items) => Value::Array(items.iter().map(strip_internal).collect()),
                Value::String(_) if PrivacyService::scan_for_prohibited_content(value).is_err() => {
                    Value::String("[REDACTED]".into())
                }
                _ => value.clone(),
            }
        }

        match parameters {
            Value::Object(map) => {
                let allowed_keys: HashSet<&str> = access_needs.iter().map(|s| s.as_str()).collect();
                let mut filtered = serde_json::Map::new();
                for (k, v) in map {
                    let k_lower = k.to_lowercase();
                    if BLOCKED_INTERNAL_KEYS
                        .iter()
                        .any(|blocked| k_lower.contains(blocked))
                    {
                        continue;
                    }
                    if allowed_keys.contains(k.as_str()) {
                        filtered.insert(k.clone(), strip_internal(v));
                    }
                }
                Value::Object(filtered)
            }
            _ => Value::Object(serde_json::Map::new()),
        }
    }

    /// Redacts sensitive keywords from output payloads before returning to consumers.
    pub fn redact_sensitive_payload(val: &Value) -> Value {
        const SENSITIVE_MATCHERS: &[&str] = &[
            "password",
            "secret",
            "token",
            "bearer",
            "api_key",
            "apikey",
            "private_key",
            "access_token",
            "client_secret",
            "credential",
            "cookie",
            "session",
            "authentication",
            "authorization",
        ];

        match val {
            Value::Object(map) => {
                let mut out = serde_json::Map::new();
                for (k, v) in map {
                    let k_lower = k.to_lowercase();
                    let short_secret_key = k_lower
                        .split(|c: char| !c.is_ascii_alphanumeric())
                        .any(|part| matches!(part, "pwd" | "auth" | "pin"));
                    if short_secret_key || SENSITIVE_MATCHERS.iter().any(|m| k_lower.contains(m)) {
                        out.insert(k.clone(), Value::String("[REDACTED]".into()));
                    } else {
                        out.insert(k.clone(), Self::redact_sensitive_payload(v));
                    }
                }
                Value::Object(out)
            }
            Value::Array(arr) => {
                Value::Array(arr.iter().map(Self::redact_sensitive_payload).collect())
            }
            Value::String(_) if PrivacyService::scan_for_prohibited_content(val).is_err() => {
                Value::String("[REDACTED]".into())
            }
            other => other.clone(),
        }
    }

    fn redact_response(raw: NormalizedResponse) -> NormalizedResponse {
        NormalizedResponse {
            status: raw.status,
            data: Self::redact_sensitive_payload(&raw.data),
            provider_reference: raw.provider_reference,
            guarantees_reported: raw
                .guarantees_reported
                .as_ref()
                .map(Self::redact_sensitive_payload),
            error_code: raw.error_code.map(|code| {
                if PrivacyService::scan_for_prohibited_content(&Value::String(code.clone()))
                    .is_err()
                {
                    "[REDACTED]".into()
                } else {
                    code
                }
            }),
            // Provider error bodies are unstructured and can contain arbitrary
            // credentials. Keep the status/code but never return the raw body.
            error_message: raw.error_message.map(|_| "[REDACTED]".into()),
        }
    }

    /// Verifies that any guarantee required by the caller is explicitly declared and supported.
    pub fn validate_guarantees(
        required: &[String],
        declared_guarantees: &Value,
    ) -> Result<(), AdapterExecutionError> {
        for req in required {
            let is_supported = match declared_guarantees {
                Value::Object(map) => map.get(req).and_then(Value::as_bool).unwrap_or(false),
                _ => false,
            };
            if !is_supported {
                return Err(AdapterExecutionError::UnsupportedGuarantee(format!(
                    "guarantee '{req}' is not supported by this integration"
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod security_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn minimization_strips_nested_internal_data_and_denies_undeclared_arguments() {
        let input = json!({
            "city": "Paris",
            "details": [{"name": "forecast", "session_token": "secret"}],
            "system_prompt": "private"
        });
        assert_eq!(ProtocolRouter::minimize_context(&input, &[]), json!({}));
        assert_eq!(
            ProtocolRouter::minimize_context(&input, &["*".into()]),
            json!({})
        );
        assert_eq!(
            ProtocolRouter::minimize_context(&input, &["details".into()]),
            json!({"details": [{"name": "forecast"}]})
        );
        assert_eq!(
            ProtocolRouter::minimize_context(&json!([input]), &["details".into()]),
            json!({})
        );
    }

    #[test]
    fn response_redaction_covers_values_and_error_text() {
        let response = NormalizedResponse {
            status: ResponseStatus::ProviderError,
            data: json!({"credential": "opaque", "message": "Bearer abcdef1234567890"}),
            provider_reference: None,
            guarantees_reported: Some(json!({"cookie": "private"})),
            error_code: Some("provider_error".into()),
            error_message: Some("raw response with unknown credential".into()),
        };
        let safe = ProtocolRouter::redact_response(response);
        assert_eq!(safe.data["credential"], "[REDACTED]");
        assert_eq!(safe.data["message"], "[REDACTED]");
        assert_eq!(safe.guarantees_reported.unwrap()["cookie"], "[REDACTED]");
        assert_eq!(safe.error_code.as_deref(), Some("provider_error"));
        assert_eq!(safe.error_message.as_deref(), Some("[REDACTED]"));
    }
}
