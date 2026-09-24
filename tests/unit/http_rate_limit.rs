use super::*;
use axum::http::HeaderValue;

#[test]
fn extracts_client_ip_with_precedence_and_splitting() {
    let mut headers = HeaderMap::new();
    assert_eq!(client_ip(&headers), "127.0.0.1");

    headers.insert("cf-connecting-ip", HeaderValue::from_static("104.28.19.42"));
    assert_eq!(client_ip(&headers), "104.28.19.42");

    headers.insert("x-real-ip", HeaderValue::from_static("192.0.2.1"));
    assert_eq!(client_ip(&headers), "192.0.2.1");

    headers.insert(
        "x-forwarded-for",
        HeaderValue::from_static("203.0.113.195, 70.41.3.18, 150.172.238.178"),
    );
    assert_eq!(client_ip(&headers), "203.0.113.195");
}

#[test]
fn trims_whitespace_in_extracted_ip() {
    let mut headers = HeaderMap::new();
    headers.insert("x-real-ip", HeaderValue::from_static("   198.51.100.99  "));
    assert_eq!(client_ip(&headers), "198.51.100.99");
}

#[test]
fn limiter_reset_and_clear_cleans_stored_keys() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 1,
        window: Duration::from_secs(60),
    });
    let d1 = limiter.check("1.2.3.4");
    assert!(d1.allowed);

    let d2 = limiter.check("1.2.3.4");
    assert!(!d2.allowed);

    limiter.clear();
    let d3 = limiter.check("1.2.3.4");
    assert!(d3.allowed);
}
