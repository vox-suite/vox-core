/**
* Integration tests checking provider feasibility contracts.
*/
const RECORD: &str = include_str!("../docs/provider-feasibility.md");

#[test]
fn provider_record_is_dated_and_covers_the_required_decision_surface() {
    for required in [
        "**Decision date:** 2026-09-19",
        "**Source access date:** 2026-09-19",
        "Authentication and access",
        "Region evidence",
        "Payment responsibility",
        "Cancellation and authoritative outcome",
        "## Required enablement gates",
        "## Implementation handoff",
    ] {
        assert!(
            RECORD.contains(required),
            "provider feasibility record is missing required evidence: {required}"
        );
    }
}

#[test]
fn every_scoped_provider_has_official_evidence_and_a_capability_decision() {
    let providers = [
        ("## Amazon", "affiliate-program.amazon.com"),
        ("## Expedia", "developers.expediagroup.com"),
        ("## Zomato", "zomato.com"),
        ("## Uber", "developer.uber.com"),
    ];

    for (provider_heading, official_domain) in providers {
        assert!(
            RECORD.contains(provider_heading),
            "provider feasibility record is missing {provider_heading}"
        );
        assert!(
            RECORD.contains(official_domain),
            "provider feasibility record is missing official evidence from {official_domain}"
        );
    }

    assert!(RECORD.contains("**L2 selected read."));
    assert!(RECORD.contains("**Conditional L3 for lodging."));
}

#[test]
fn unsupported_execution_is_explicitly_a_handoff() {
    for unsupported in [
        "Amazon consumer purchase",
        "Zomato consumer order",
        "Uber ride request",
    ] {
        assert!(
            RECORD.contains(unsupported),
            "provider feasibility record does not classify {unsupported}"
        );
    }

    assert!(RECORD.contains("must be declared unavailable and represented as labelled handoff"));
    assert!(RECORD.contains("They cannot reinterpret handoff as execution."));
}
