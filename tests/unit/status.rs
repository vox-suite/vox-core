use super::*;
#[test]
fn webhook_endpoint_requires_plain_https() {
    assert!(webhook_endpoint("https://host.example/status").is_ok());
    assert!(webhook_endpoint("http://host.example/status").is_err());
    assert!(webhook_endpoint("https://user@host.example/status").is_err());
    assert!(webhook_endpoint("https://127.0.0.1/status").is_err());
    assert!(webhook_endpoint("https://0.0.0.0/status").is_err());
    assert!(webhook_endpoint("https://100.64.0.1/status").is_err());
    assert!(webhook_endpoint("https://[::1]/status").is_err());
    assert!(webhook_endpoint("https://localhost/status").is_err());
}
#[test]
fn webhook_signature_binds_timestamp_and_body() {
    let signature = webhook_signature("secret", "123", b"{} ").unwrap();
    assert_ne!(
        signature,
        webhook_signature("secret", "124", b"{} ").unwrap()
    );
    assert_ne!(
        signature,
        webhook_signature("secret", "123", b"{}").unwrap()
    );
}
