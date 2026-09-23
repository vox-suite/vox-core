/**
* Integration tests verifying platform conformance contracts.
*/
use vox_core::conformance::{
    Command, ErrorCategory, JsonBoundary, OutcomeStatus, PlatformAdapter, ReferencePlatform,
    ResultKind, SemanticError, SemanticResult, canonical_suite, parse_suite, run_suite,
};

#[test]
fn canonical_fixture_conforms_in_process() {
    let report = run_suite(&canonical_suite(), ReferencePlatform::default);
    assert!(
        report.is_conformant(),
        "semantic failures: {:#?}",
        report.failures
    );
    assert_eq!(report.scenarios, 4);
    assert_eq!(report.steps, 39);
}

#[test]
fn canonical_fixture_conforms_through_the_json_boundary() {
    let report = run_suite(&canonical_suite(), || {
        JsonBoundary::new(ReferencePlatform::default())
    });
    assert!(
        report.is_conformant(),
        "boundary failures: {:#?}",
        report.failures
    );
}

#[test]
fn fixture_parser_rejects_unknown_versions_and_ambiguous_expectations() {
    let wrong_version = r#"{"fixture_version":2,"scenarios":[]}"#;
    assert!(parse_suite(wrong_version).is_err());

    let ambiguous = r#"{
      "fixture_version": 1,
      "scenarios": [{
        "id": "invalid",
        "purpose": "invalid",
        "steps": [{
          "command": {"command":"read_events","context_id":"user-a"},
          "expect": {"result":"events_read","error":"not_found"}
        }]
      }]
    }"#;
    assert!(parse_suite(ambiguous).is_err());
}

struct ViolationAdapter {
    inner: ReferencePlatform,
    violation: Violation,
}

enum Violation {
    IgnoreIsolation,
    IgnoreApprovalBinding,
    InventSuccess,
}

impl ViolationAdapter {
    fn new(violation: Violation) -> Self {
        Self {
            inner: ReferencePlatform::default(),
            violation,
        }
    }
}

impl PlatformAdapter for ViolationAdapter {
    fn apply(&mut self, command: Command) -> Result<SemanticResult, SemanticError> {
        let result = self.inner.apply(command);
        match (&self.violation, result) {
            (
                Violation::IgnoreIsolation,
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
                Violation::IgnoreApprovalBinding,
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
            (Violation::InventSuccess, Ok(mut result))
                if result.kind == ResultKind::ProposalExecuted
                    && result.outcome == Some(OutcomeStatus::Unknown) =>
            {
                result.outcome = Some(OutcomeStatus::Succeeded);
                Ok(result)
            }
            (_, result) => result,
        }
    }
}

#[test]
fn suite_detects_user_context_isolation_violations() {
    let report = run_suite(&canonical_suite(), || {
        ViolationAdapter::new(Violation::IgnoreIsolation)
    });
    assert!(!report.is_conformant());
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.scenario_id == "user-context-isolation")
    );
}

#[test]
fn suite_detects_approval_binding_violations() {
    let report = run_suite(&canonical_suite(), || {
        ViolationAdapter::new(Violation::IgnoreApprovalBinding)
    });
    assert!(!report.is_conformant());
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.scenario_id == "exact-approval-binding")
    );
}

#[test]
fn suite_detects_untruthful_outcomes() {
    let report = run_suite(&canonical_suite(), || {
        ViolationAdapter::new(Violation::InventSuccess)
    });
    assert!(!report.is_conformant());
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.scenario_id == "truthful-and-idempotent-outcomes")
    );
}
