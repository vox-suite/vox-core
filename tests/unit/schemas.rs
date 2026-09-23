use super::compile_schema;
use serde_json::json;

#[test]
fn rejects_http_schema_references() {
    let schema = json!({"$ref": "https://example.com/schema.json"});
    let error = compile_schema(&schema).expect_err("external ref");
    assert!(error.contains("external schema reference"));
}

#[test]
fn rejects_file_schema_references() {
    let schema = json!({"$ref": "file:///tmp/schema.json"});
    assert!(compile_schema(&schema).is_err());
}

#[test]
fn allows_local_fragment_references() {
    let schema = json!({
        "type": "object",
        "properties": { "name": { "$ref": "#/$defs/name" } },
        "$defs": { "name": { "type": "string" } }
    });
    let validator = compile_schema(&schema).expect("local schema");
    assert!(validator.is_valid(&json!({"name": "Ada"})));
    assert!(!validator.is_valid(&json!({"name": 1})));
}
