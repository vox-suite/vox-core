/**
 * Isolated execution sandbox for untrusted scripts and tools.
 */

use crate::execution::{AdapterOutcome, AdapterRequest, ExecutionAdapter};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scenario {
    Success,
    Rejection,
    PriceChanged,
    ProviderAuthentication,
    Timeout,
    DuplicateDelivery,
    Cancellation,
    Refund,
    CrashAfterDispatch,
    ReconcileSuccess,
    ReconcileFailure,
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SandboxSnapshot {
    scripts: HashMap<String, Scenario>,
    dispatches: HashMap<String, u32>,
    effects: HashMap<String, u32>,
    refunds: HashMap<String, Value>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RefundObservation {
    pub provider_reference: String,
    pub evidence: Value,
}

#[derive(Clone, Default)]
pub struct TransactionalSandbox {
    state: Arc<Mutex<SandboxSnapshot>>,
}

impl TransactionalSandbox {
    pub fn seeded(seed: impl Into<String>, scenario: Scenario) -> Self {
        let sandbox = Self::default();
        sandbox.script(seed, scenario);
        sandbox
    }

    pub fn from_snapshot(snapshot: SandboxSnapshot) -> Self {
        Self {
            state: Arc::new(Mutex::new(snapshot)),
        }
    }

    pub fn snapshot(&self) -> SandboxSnapshot {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .clone()
    }

    pub fn script(&self, key: impl Into<String>, scenario: Scenario) {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .scripts
            .insert(key.into(), scenario);
    }

    pub fn dispatch_count(&self, key: &str) -> u32 {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .dispatches
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    pub fn effect_count(&self, provider_reference: &str) -> u32 {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .effects
            .get(provider_reference)
            .copied()
            .unwrap_or_default()
    }

    pub fn refund(&self, provider_reference: &str) -> RefundObservation {
        let evidence = json!({
            "sandbox": true,
            "kind": "refund",
            "receipt": format!("refund:{provider_reference}"),
        });
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .refunds
            .insert(provider_reference.into(), evidence.clone());
        RefundObservation {
            provider_reference: provider_reference.into(),
            evidence,
        }
    }

    pub fn refund_evidence(&self, provider_reference: &str) -> Option<Value> {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .refunds
            .get(provider_reference)
            .cloned()
    }

    fn scenario(&self, key: &str) -> Scenario {
        self.state
            .lock()
            .expect("sandbox state lock poisoned")
            .scripts
            .get(key)
            .cloned()
            .unwrap_or(Scenario::Unknown)
    }

    fn reference(key: &str) -> String {
        format!("sandbox:{key}")
    }

    fn receipt(key: &str, kind: &str) -> Value {
        json!({"sandbox": true, "kind": kind, "receipt": format!("{kind}:{key}")})
    }
}

#[async_trait::async_trait]
impl ExecutionAdapter for TransactionalSandbox {
    async fn dispatch(&self, request: AdapterRequest) -> AdapterOutcome {
        let scenario = self.scenario(&request.idempotency_key);
        let mut state = self.state.lock().expect("sandbox state lock poisoned");
        *state
            .dispatches
            .entry(request.idempotency_key.clone())
            .or_default() += 1;
        let reference = Self::reference(&request.idempotency_key);
        if matches!(
            scenario,
            Scenario::Success | Scenario::DuplicateDelivery | Scenario::Refund
        ) {
            state.effects.entry(reference.clone()).or_insert(1);
        }
        drop(state);

        match scenario {
            Scenario::Success | Scenario::DuplicateDelivery | Scenario::Refund => {
                AdapterOutcome::Succeeded {
                    provider_reference: reference,
                    evidence: Self::receipt(&request.idempotency_key, "receipt"),
                }
            }
            Scenario::Rejection => AdapterOutcome::Failed {
                code: "sandbox_rejected".into(),
            },
            Scenario::PriceChanged => AdapterOutcome::Failed {
                code: "sandbox_fresh_proposal_required".into(),
            },
            Scenario::ProviderAuthentication => AdapterOutcome::AwaitingProviderAuthentication {
                provider_reference: Some(reference),
            },
            Scenario::Cancellation => AdapterOutcome::Cancelled {
                provider_reference: Some(reference),
                evidence: Self::receipt(&request.idempotency_key, "cancellation"),
            },
            Scenario::Timeout | Scenario::CrashAfterDispatch | Scenario::Unknown => {
                AdapterOutcome::Unknown {
                    provider_reference: Some(reference),
                    code: "sandbox_unknown".into(),
                }
            }
            Scenario::ReconcileSuccess | Scenario::ReconcileFailure => {
                AdapterOutcome::Reconciling {
                    provider_reference: Some(reference),
                }
            }
        }
    }

    async fn reconcile(
        &self,
        request: AdapterRequest,
        _provider_reference: Option<&str>,
    ) -> AdapterOutcome {
        match self.scenario(&request.idempotency_key) {
            Scenario::ReconcileSuccess => AdapterOutcome::Succeeded {
                provider_reference: Self::reference(&request.idempotency_key),
                evidence: Self::receipt(&request.idempotency_key, "reconciled"),
            },
            Scenario::ReconcileFailure => AdapterOutcome::Failed {
                code: "sandbox_reconciled_failure".into(),
            },
            _ => AdapterOutcome::Unknown {
                provider_reference: Some(Self::reference(&request.idempotency_key)),
                code: "sandbox_unresolved".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::AdapterRequest;
    use crate::execution_policy::ExecutionIdentity;
    use uuid::Uuid;

    fn request(key: &str) -> AdapterRequest {
        AdapterRequest {
            execution_id: Uuid::nil(),
            idempotency_key: key.into(),
            capability_external_key: "sandbox.write".into(),
            identity: ExecutionIdentity {
                provider_external_key: "sandbox".into(),
                model_identifier: "test".into(),
                account_reference: "synthetic".into(),
                connection_id: Uuid::nil(),
                price_amount_minor: 0,
                price_currency: "USD".into(),
            },
        }
    }

    #[tokio::test]
    async fn restart_restores_script_and_provider_idempotency_identity() {
        let sandbox = TransactionalSandbox::seeded("seed", Scenario::DuplicateDelivery);
        let first = sandbox.dispatch(request("seed")).await;
        let restarted = TransactionalSandbox::from_snapshot(sandbox.snapshot());
        let second = restarted.dispatch(request("seed")).await;
        assert!(
            matches!(first, AdapterOutcome::Succeeded { ref provider_reference, .. } if provider_reference == "sandbox:seed")
        );
        assert!(
            matches!(second, AdapterOutcome::Succeeded { ref provider_reference, .. } if provider_reference == "sandbox:seed")
        );
        assert_eq!(restarted.dispatch_count("seed"), 2);
        assert_eq!(restarted.effect_count("sandbox:seed"), 1);
    }

    #[tokio::test]
    async fn timeout_reconciliation_does_not_invent_success() {
        let sandbox = TransactionalSandbox::seeded("seed", Scenario::Timeout);
        assert!(matches!(
            sandbox.dispatch(request("seed")).await,
            AdapterOutcome::Unknown { .. }
        ));
        assert!(matches!(
            sandbox.reconcile(request("seed"), None).await,
            AdapterOutcome::Unknown { .. }
        ));
    }

    #[tokio::test]
    async fn refund_is_separate_from_success_evidence() {
        let sandbox = TransactionalSandbox::seeded("seed", Scenario::Refund);
        let outcome = sandbox.dispatch(request("seed")).await;
        let provider_reference = match outcome {
            AdapterOutcome::Succeeded {
                provider_reference,
                evidence,
            } => {
                assert_eq!(evidence["kind"], "receipt");
                provider_reference
            }
            _ => panic!("expected success"),
        };
        let refund = sandbox.refund(&provider_reference);
        assert_eq!(refund.provider_reference, provider_reference);
        assert_eq!(refund.evidence["kind"], "refund");
        let restarted = TransactionalSandbox::from_snapshot(sandbox.snapshot());
        assert_eq!(
            restarted.refund_evidence(&provider_reference),
            Some(refund.evidence)
        );
    }
}
