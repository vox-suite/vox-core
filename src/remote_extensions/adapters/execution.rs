use super::{
    AdapterExecutionError, ExtensionInvocation, ExtensionReconciliation, ProtocolRouter,
    ResponseStatus,
};
use crate::{
    execution::{AdapterOutcome, AdapterRequest, ExecutionAdapter},
    identity::ResolvedUserContext,
    remote_extensions::RemoteExtensionService,
};
use serde_json::Value;
use std::sync::Arc;
use uuid::Uuid;

pub struct ExtensionExecutionAdapter {
    extension_service: Arc<RemoteExtensionService>,
    router: Arc<ProtocolRouter>,
    extension_id: Uuid,
    context: ResolvedUserContext,
    parameters: Value,
    required_guarantees: Vec<String>,
}

impl ExtensionExecutionAdapter {
    pub fn new(
        extension_service: Arc<RemoteExtensionService>,
        router: Arc<ProtocolRouter>,
        extension_id: Uuid,
        context: ResolvedUserContext,
        parameters: Value,
    ) -> Self {
        Self {
            extension_service,
            router,
            extension_id,
            context,
            parameters,
            required_guarantees: Vec::new(),
        }
    }

    pub fn with_guarantees(mut self, guarantees: Vec<String>) -> Self {
        self.required_guarantees = guarantees;
        self
    }
}

#[async_trait::async_trait]
impl ExecutionAdapter for ExtensionExecutionAdapter {
    async fn dispatch(&self, request: AdapterRequest) -> AdapterOutcome {
        // Authorize consequential call: verifies active state, consent, conformance, and operator enablement
        let authorized_endpoint = match self
            .extension_service
            .authorize_call(
                &self.context,
                self.extension_id,
                &request.capability_external_key,
            )
            .await
        {
            Ok(endpoint) => endpoint,
            Err(e) => {
                return AdapterOutcome::Failed {
                    code: format!("AUTHORIZATION_DENIED_{e:?}"),
                };
            }
        };

        let invocation = ExtensionInvocation {
            capability_key: request.capability_external_key.clone(),
            parameters: self.parameters.clone(),
            access_context: Value::Null,
            idempotency_key: Some(request.idempotency_key.clone()),
            execution_id: Some(request.execution_id),
            required_guarantees: self.required_guarantees.clone(),
        };

        match self.router.execute(&authorized_endpoint, &invocation).await {
            Ok(normalized) => match normalized.status {
                ResponseStatus::Success => AdapterOutcome::Succeeded {
                    provider_reference: normalized
                        .provider_reference
                        .unwrap_or_else(|| request.idempotency_key.clone()),
                    evidence: normalized.data,
                },
                ResponseStatus::ClientError => AdapterOutcome::Failed {
                    code: normalized
                        .error_code
                        .unwrap_or_else(|| "CLIENT_ERROR".into()),
                },
                ResponseStatus::ProviderError => AdapterOutcome::Failed {
                    code: normalized
                        .error_code
                        .unwrap_or_else(|| "PROVIDER_ERROR".into()),
                },
                ResponseStatus::Timeout => AdapterOutcome::Reconciling {
                    provider_reference: normalized.provider_reference,
                },
                ResponseStatus::Uncertain => AdapterOutcome::Unknown {
                    provider_reference: normalized.provider_reference,
                    code: normalized.error_code.unwrap_or_else(|| "UNCERTAIN".into()),
                },
            },
            Err(AdapterExecutionError::AdapterDisabled(p)) => AdapterOutcome::Failed {
                code: format!("ADAPTER_DISABLED_{p:?}"),
            },
            Err(AdapterExecutionError::UnsupportedGuarantee(g)) => AdapterOutcome::Failed {
                code: format!("UNSUPPORTED_GUARANTEE_{g}"),
            },
            Err(AdapterExecutionError::Timeout(_)) => AdapterOutcome::Reconciling {
                provider_reference: None,
            },
            Err(e) => AdapterOutcome::Unknown {
                provider_reference: None,
                code: format!("EXECUTION_ERROR_{e:?}"),
            },
        }
    }

    async fn reconcile(
        &self,
        request: AdapterRequest,
        provider_reference: Option<&str>,
    ) -> AdapterOutcome {
        let authorized_endpoint = match self
            .extension_service
            .authorize_call(
                &self.context,
                self.extension_id,
                &request.capability_external_key,
            )
            .await
        {
            Ok(endpoint) => endpoint,
            Err(e) => {
                return AdapterOutcome::Unknown {
                    provider_reference: provider_reference.map(String::from),
                    code: format!("AUTHORIZATION_DENIED_{e:?}"),
                };
            }
        };

        let reconciliation = ExtensionReconciliation {
            capability_key: request.capability_external_key.clone(),
            execution_id: request.execution_id,
            idempotency_key: request.idempotency_key.clone(),
            provider_reference: provider_reference.map(String::from),
        };

        match self
            .router
            .reconcile(&authorized_endpoint, &reconciliation)
            .await
        {
            Ok(normalized) => match normalized.status {
                ResponseStatus::Success => AdapterOutcome::Succeeded {
                    provider_reference: normalized
                        .provider_reference
                        .or_else(|| provider_reference.map(String::from))
                        .unwrap_or_else(|| request.idempotency_key.clone()),
                    evidence: normalized.data,
                },
                ResponseStatus::ClientError | ResponseStatus::ProviderError => {
                    AdapterOutcome::Failed {
                        code: normalized
                            .error_code
                            .unwrap_or_else(|| "RECONCILE_FAILED".into()),
                    }
                }
                ResponseStatus::Timeout | ResponseStatus::Uncertain => {
                    AdapterOutcome::Reconciling {
                        provider_reference: normalized
                            .provider_reference
                            .or_else(|| provider_reference.map(String::from)),
                    }
                }
            },
            Err(e) => AdapterOutcome::Unknown {
                provider_reference: provider_reference.map(String::from),
                code: format!("RECONCILE_ERROR_{e:?}"),
            },
        }
    }
}
