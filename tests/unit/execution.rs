use super::confirmation_evidence_is_present;
#[test]
fn confirmation_evidence_must_be_a_non_empty_object() {
    assert!(!confirmation_evidence_is_present(&serde_json::json!(null)));
    assert!(!confirmation_evidence_is_present(&serde_json::json!({})));
    assert!(!confirmation_evidence_is_present(&serde_json::json!(
        "receipt"
    )));
    assert!(confirmation_evidence_is_present(
        &serde_json::json!({"receipt":"synthetic"})
    ));
}
