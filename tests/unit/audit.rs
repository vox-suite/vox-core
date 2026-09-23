use super::safe_details;
#[test]
fn rejects_secret_like_or_nested_admin_details() {
    assert!(safe_details(
        &serde_json::json!({"count": 1, "operation": "list"})
    ));
    assert!(!safe_details(&serde_json::json!({"api_token": "canary"})));
    assert!(!safe_details(&serde_json::json!({"nested": {"x": 1}})));
}
