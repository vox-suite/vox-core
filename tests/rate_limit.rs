/**
* Integration tests for HTTP rate limiting and client throttling.
*/
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use std::time::Duration;
use tower::ServiceExt;
use vox_core::http::{
    AppState,
    rate_limit::{RateLimitConfig, RateLimiter},
    router,
};

#[tokio::test]
async fn rate_limiting_allows_requests_within_limit_and_attaches_headers() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 3,
        window: Duration::from_secs(60),
    });
    let state = AppState::new(true).with_rate_limiter(limiter);
    let app = router(state);

    let res = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-forwarded-for", "203.0.113.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-ratelimit-limit")
            .unwrap()
            .to_str()
            .unwrap(),
        "3"
    );
    assert_eq!(
        res.headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "2"
    );
    assert!(res.headers().contains_key("x-ratelimit-reset"));

    let res = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-forwarded-for", "203.0.113.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "1"
    );

    let res = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-forwarded-for", "203.0.113.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    assert_eq!(
        res.headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "0"
    );

    let res = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-forwarded-for", "203.0.113.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        res.headers()
            .get("x-ratelimit-limit")
            .unwrap()
            .to_str()
            .unwrap(),
        "3"
    );
    assert_eq!(
        res.headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "0"
    );
    assert!(res.headers().contains_key("retry-after"));
    assert!(res.headers().contains_key("x-ratelimit-reset"));

    let body_bytes = axum::body::to_bytes(res.into_body(), 1024).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(json["error"], "Too many requests. Please try again later.");
}

#[tokio::test]
async fn rate_limiting_isolates_different_client_ips() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 2,
        window: Duration::from_secs(60),
    });
    let state = AppState::new(true).with_rate_limiter(limiter);
    let app = router(state);

    for _ in 0..2 {
        let res = app
            .clone()
            .oneshot(
                Request::get("/health/live")
                    .header("x-forwarded-for", "198.51.100.1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    let res_a = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-forwarded-for", "198.51.100.1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_a.status(), StatusCode::TOO_MANY_REQUESTS);

    let res_b = app
        .clone()
        .oneshot(
            Request::get("/health/live")
                .header("x-real-ip", "198.51.100.2")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res_b.status(), StatusCode::OK);
    assert_eq!(
        res_b
            .headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "1"
    );
}

#[tokio::test]
async fn rate_limiting_applies_to_v1_routes_as_well() {
    let limiter = RateLimiter::new(RateLimitConfig {
        max_requests: 1,
        window: Duration::from_secs(60),
    });
    let state = AppState::new(true).with_rate_limiter(limiter);
    let app = router(state);

    let res1 = app
        .clone()
        .oneshot(
            Request::get("/v1/admin/redis?prefix=test")
                .header("cf-connecting-ip", "10.10.10.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_ne!(res1.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        res1.headers()
            .get("x-ratelimit-remaining")
            .unwrap()
            .to_str()
            .unwrap(),
        "0"
    );

    let res2 = app
        .clone()
        .oneshot(
            Request::get("/v1/admin/redis?prefix=test")
                .header("cf-connecting-ip", "10.10.10.10")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res2.status(), StatusCode::TOO_MANY_REQUESTS);
}
