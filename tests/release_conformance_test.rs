/**
 * Platform Security & Conformance Release Suite (E50 / vox-core#30).
 *
 * Verifies Platform V1 criteria:
 * 1. All Core acceptance scenarios and security gates pass in a clean environment.
 * 2. Credential scanning finds no secrets in model context, client payloads, logs, traces, exports, or audit evidence.
 * 3. Every failure includes reproducible evidence and blocks release until resolved.
 * 4. Isolation, authority, exact approval binding, and truthful outcomes are provably enforced.
 */
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::json;
use tower::ServiceExt;

use vox_core::{
    conformance::{
        Command, ErrorCategory, JsonBoundary, OutcomeStatus, PlatformAdapter, ReferencePlatform,
        ResultKind, SemanticError, SemanticResult, canonical_suite, run_suite,
    },
    http::{AppState, router},
    privacy::{CANONICAL_DELETION_DISCLOSURE, PREFERENCE_AUTHORITY_DISCLAIMER, PrivacyService},
};

// ==============================================================================
// 1. CANONICAL PLATFORM CONFORMANCE SUITE GATES
// ==============================================================================

#[test]
fn test_canonical_conformance_suite_passes_all_scenarios() {
    let report = run_suite(&canonical_suite(), ReferencePlatform::default);
    assert!(
        report.is_conformant(),
        "Platform conformance suite failed with errors: {:#?}",
        report.failures
    );
    assert_eq!(report.scenarios, 4);
    assert_eq!(report.steps, 39);
    assert!(report.failures.is_empty());
}

#[test]
fn test_boundary_conformance_suite_passes_through_json_wire_protocol() {
    let report = run_suite(&canonical_suite(), || {
        JsonBoundary::new(ReferencePlatform::default())
    });
    assert!(
        report.is_conformant(),
        "JSON boundary conformance failed: {:#?}",
        report.failures
    );
    assert_eq!(report.scenarios, 4);
    assert_eq!(report.steps, 39);
}

// ==============================================================================
// 2. SECURITY GATES: ISOLATION, AUTHORITY, APPROVAL BINDING, & TRUTHFUL OUTCOMES
// ==============================================================================

struct InjectedViolationAdapter {
    inner: ReferencePlatform,
    violation: ViolationKind,
}

enum ViolationKind {
    BypassTenantIsolation,
    BypassExactApprovalBinding,
    InventOutcomeSuccess,
}

impl InjectedViolationAdapter {
    fn new(kind: ViolationKind) -> Self {
        Self {
            inner: ReferencePlatform::default(),
            violation: kind,
        }
    }
}

impl PlatformAdapter for InjectedViolationAdapter {
    fn apply(&mut self, command: Command) -> Result<SemanticResult, SemanticError> {
        let result = self.inner.apply(command);
        match (&self.violation, result) {
            (
                ViolationKind::BypassTenantIsolation,
                Err(SemanticError {
                    category: ErrorCategory::IsolationViolation,
                    ..
                }),
            ) => Ok(SemanticResult {
                kind: ResultKind::ProposalApproved,
                action_id: None,
                outcome: None,
                capabilities: Vec::new(),
                events: Vec::new(),
            }),
            (
                ViolationKind::BypassExactApprovalBinding,
                Err(SemanticError {
                    category: ErrorCategory::ApprovalMismatch,
                    ..
                }),
            ) => Ok(SemanticResult {
                kind: ResultKind::ProposalApproved,
                action_id: None,
                outcome: None,
                capabilities: Vec::new(),
                events: Vec::new(),
            }),
            (ViolationKind::InventOutcomeSuccess, Ok(mut res))
                if res.kind == ResultKind::ProposalExecuted
                    && res.outcome == Some(OutcomeStatus::Unknown) =>
            {
                res.outcome = Some(OutcomeStatus::Succeeded);
                Ok(res)
            }
            (_, result) => result,
        }
    }
}

#[test]
fn test_security_gate_detects_tenant_isolation_violations() {
    let report = run_suite(&canonical_suite(), || {
        InjectedViolationAdapter::new(ViolationKind::BypassTenantIsolation)
    });
    assert!(
        !report.is_conformant(),
        "Security gate must fail closed when tenant isolation is violated"
    );
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.scenario_id == "user-context-isolation"),
        "Report must specifically identify user-context-isolation failure"
    );
}

#[test]
fn test_security_gate_detects_tampered_approval_binding_violations() {
    let report = run_suite(&canonical_suite(), || {
        InjectedViolationAdapter::new(ViolationKind::BypassExactApprovalBinding)
    });
    assert!(
        !report.is_conformant(),
        "Security gate must fail closed when proposal binding is bypassed"
    );
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.scenario_id == "exact-approval-binding"),
        "Report must specifically identify exact-approval-binding failure"
    );
}

#[test]
fn test_security_gate_detects_untruthful_outcomes() {
    let report = run_suite(&canonical_suite(), || {
        InjectedViolationAdapter::new(ViolationKind::InventOutcomeSuccess)
    });
    assert!(
        !report.is_conformant(),
        "Security gate must fail closed when success is invented without provider evidence"
    );
    assert!(
        report
            .failures
            .iter()
            .any(|f| f.scenario_id == "truthful-and-idempotent-outcomes"),
        "Report must specifically identify truthful-and-idempotent-outcomes failure"
    );
}

// ==============================================================================
// 3. ZERO-LEAKAGE CREDENTIAL SCANNING GATES
// ==============================================================================

#[test]
fn test_credential_scanner_rejects_api_keys_and_secret_tokens() {
    // 1. Anthropic/OpenAI API key prefix
    let payload_with_key = json!({
        "agent": "saathi",
        "parameters": {
            "api_key": "sk-ant-api03-abcdef1234567890abcdef1234567890"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_key).is_err(),
        "Scanner must reject payloads with sk- API keys"
    );

    // 2. Vox secret key prefix
    let payload_with_vox_key = json!({
        "credentials": {
            "token": "vox_sk_live_abcdef1234567890"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_vox_key).is_err(),
        "Scanner must reject payloads with vox_sk_ keys"
    );

    // 3. Bearer token in string value
    let payload_with_bearer = json!({
        "headers": {
            "auth": "Bearer eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.e30.t-IDcSemACt8x4iTMCda8Yhe3iZaWbvV5XKSTbuAn0M"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_bearer).is_err(),
        "Scanner must reject Bearer/JWT tokens"
    );
}

#[test]
fn test_credential_scanner_rejects_private_keys_and_payment_cards() {
    // 1. Private key header
    let payload_with_private_key = json!({
        "host_config": {
            "key_material": "-----BEGIN PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC...\n-----END PRIVATE KEY-----"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_private_key).is_err(),
        "Scanner must reject private keys"
    );

    // 2. Prohibited payment credential keys
    let payload_with_card = json!({
        "payment": {
            "card_number": "4111222233334444",
            "cvv": "123"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_card).is_err(),
        "Scanner must reject card numbers and cvv keys"
    );

    // 3. Password or secret key names
    let payload_with_password = json!({
        "auth": {
            "client_secret": "super-secret-pass-99"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&payload_with_password).is_err(),
        "Scanner must reject client_secret and passwords"
    );
}

#[test]
fn test_credential_scanner_allows_clean_portable_export_bundle() {
    let clean_bundle = json!({
        "schema_version": "1.0",
        "categories": ["config", "preferences"],
        "config": {
            "agents": [{
                "external_key": "saathi",
                "display_name": "Saathi Personal Assistant",
                "description": "General personal assistant"
            }],
            "integrations": [{
                "external_key": "uber",
                "display_name": "Uber Rides",
                "protocol": "direct"
            }],
            "connections": [{
                "integration_key": "uber",
                "account_display_id": "rider-alice",
                "credential_custody": "platform_held",
                "requires_renewed_authorization": true
            }]
        },
        "preferences": [{
            "category": "dietary",
            "preference_key": "vegetarian",
            "value": "strict",
            "is_sensitive": false,
            "authority_disclaimer": PREFERENCE_AUTHORITY_DISCLAIMER
        }]
    });

    let scan_result = PrivacyService::scan_for_prohibited_content(&clean_bundle);
    assert!(
        scan_result.is_ok(),
        "Clean portable export must pass scanner without false positives: {scan_result:?}"
    );
}

// ==============================================================================
// 4. HTTP SECURITY GATE: AUTHENTICATED ENDPOINTS FAIL CLOSED
// ==============================================================================

#[tokio::test]
async fn test_retention_prune_requires_service_token_fails_closed() {
    let state = AppState::new(true);
    let app = router(state);

    // Unauthenticated POST /v1/privacy/retention/prune must return 401 Unauthorized
    let unauthed_req = Request::builder()
        .method("POST")
        .uri("/v1/privacy/retention/prune")
        .body(Body::empty())
        .unwrap();

    let res_unauthed = app.clone().oneshot(unauthed_req).await.unwrap();
    assert_eq!(
        res_unauthed.status(),
        StatusCode::UNAUTHORIZED,
        "POST /v1/privacy/retention/prune must fail closed with 401 when unauthenticated"
    );

    // Request with wrong token must return 401 Unauthorized
    let wrong_req = Request::builder()
        .method("POST")
        .uri("/v1/privacy/retention/prune")
        .header("authorization", "Bearer invalid-service-token")
        .body(Body::empty())
        .unwrap();

    let res_wrong = app.clone().oneshot(wrong_req).await.unwrap();
    assert_eq!(
        res_wrong.status(),
        StatusCode::UNAUTHORIZED,
        "POST /v1/privacy/retention/prune must reject wrong service token with 401"
    );
}

// ==============================================================================
// 5. INVARIANTS & CANONICAL DISCLOSURES VERIFICATION
// ==============================================================================

#[test]
fn test_canonical_deletion_disclosure_and_invariants() {
    let disclosure = CANONICAL_DELETION_DISCLOSURE;

    // Discloses platform-controlled removal
    assert!(disclosure.contains("Platform task entries and conversation turns are removed"));

    // Discloses legal audit hold retention window (up to 365 days)
    assert!(disclosure.contains("365 days"));

    // Discloses disaster recovery backup window (up to 30 days)
    assert!(disclosure.contains("30 days"));

    // Invariant 4: Universal deletion and undo non-claim
    assert!(disclosure.contains("does not cancel or refund completed external transactions"));
    assert!(disclosure.contains("beyond immediate platform deletion"));
}

#[test]
fn test_preference_authority_disclaimer_invariant() {
    let disclaimer = PREFERENCE_AUTHORITY_DISCLAIMER;

    // Invariant 3: Sensitive preference confirmation and zero authority transfer
    assert!(disclaimer.contains("User preference is advisory context only"));
    assert!(disclaimer.contains("It confers no execution authority"));
    assert!(disclaimer.contains(
        "Provider currency, timezone, and inventory facts remain strictly authoritative"
    ));
}

#[test]
fn test_credential_scanner_verifies_model_context_and_audit_traces() {
    // 1. Model context with injected secrets is rejected
    let tainted_model_context = json!({
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Book hotel with sk_live_998877665544332211"}
        ]
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&tainted_model_context).is_err(),
        "Model context containing API keys must be rejected"
    );

    // 2. Audit evidence details with leaked session_id or tokens is rejected
    let tainted_audit_evidence = json!({
        "event_type": "execution.completed",
        "details": {
            "session_id": "sess_live_12345",
            "provider": "expedia"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&tainted_audit_evidence).is_err(),
        "Audit evidence containing session_id must be rejected"
    );

    // 3. Clean audit evidence and sanitized model context passes scan
    let clean_model_context = json!({
        "messages": [
            {"role": "system", "content": "You are a helpful assistant."},
            {"role": "user", "content": "Find hotels in Seattle for next weekend."}
        ]
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&clean_model_context).is_ok(),
        "Clean model context must pass scanning"
    );

    let clean_audit_evidence = json!({
        "event_type": "execution.completed",
        "details": {
            "action": "book_lodging",
            "provider": "expedia",
            "status": "succeeded"
        }
    });
    assert!(
        PrivacyService::scan_for_prohibited_content(&clean_audit_evidence).is_ok(),
        "Clean audit evidence must pass scanning"
    );
}
