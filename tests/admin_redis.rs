use axum::{body::Body, http::Request};
use tower::ServiceExt;
use vox_core::http::{AppState, router};

#[tokio::test]
async fn admin_is_closed_without_configuration() {
    let app = router(AppState::new(true));
    let response = app
        .oneshot(Request::get("/v1/admin/redis").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
}

fn configured(url: Option<&str>, token: &str) -> axum::Router {
    router(
        AppState::new(true)
            .with_admin(vox_core::http::admin::RedisAdmin::new(url, token.into()).unwrap()),
    )
}

#[tokio::test]
async fn dedicated_token_is_required_and_unavailable_redis_is_explicit() {
    let app = configured(None, "admin-only");
    for token in ["", "service-token", "wrong"] {
        let response = app
            .clone()
            .oneshot(
                Request::get("/v1/admin/redis")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }
    let response = app
        .oneshot(
            Request::get("/v1/admin/redis")
                .header("authorization", "Bearer admin-only")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(response.headers()["cache-control"], "no-store");
}

#[tokio::test]
async fn empty_token_cannot_enable_admin_and_mutations_are_not_routed() {
    let response = configured(None, "")
        .oneshot(
            Request::get("/v1/admin/redis")
                .header("authorization", "Bearer ")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    for method in ["POST", "PUT", "PATCH", "DELETE"] {
        let response = configured(None, "admin-only")
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri("/v1/admin/redis")
                    .header("authorization", "Bearer admin-only")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 405);
    }
}

#[tokio::test]
async fn rejects_invalid_queries_before_connecting() {
    for query in [
        "cursor=-1",
        "cursor=18446744073709551616",
        "cursor=1.2",
        "key=",
        "key=a%00b",
    ] {
        let response = configured(None, "admin-only")
            .oneshot(
                Request::get(format!("/v1/admin/redis?{query}"))
                    .header("authorization", "Bearer admin-only")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
    }
}

#[tokio::test]
#[ignore = "requires isolated Redis at TEST_REDIS_URL"]
async fn reads_real_redis_without_mutation_with_bounded_previews() {
    use axum::body::to_bytes;
    use redis::AsyncCommands;
    let url = std::env::var("TEST_REDIS_URL").expect("isolated TEST_REDIS_URL");
    let client = redis::Client::open(url.as_str()).unwrap();
    let mut connection = client.get_multiplexed_async_connection().await.unwrap();
    let prefix = format!("vox:admin-test:{}", uuid::Uuid::new_v4());
    let string_key = format!("{prefix}:string");
    let large_key = format!("{prefix}:large");
    let list_key = format!("{prefix}:list");
    let hash_key = format!("{prefix}:hash");
    let set_key = format!("{prefix}:set");
    let zset_key = format!("{prefix}:zset");
    let stream_key = format!("{prefix}:stream");
    let _: () = connection
        .set_ex(&string_key, r#"{"name":"Fixture"}"#, 300)
        .await
        .unwrap();
    let _: () = connection
        .set(&large_key, "x".repeat(100_000))
        .await
        .unwrap();
    let _: () = connection
        .rpush(&list_key, vec!["one", "two"])
        .await
        .unwrap();
    let _: () = connection.hset(&hash_key, "field", "value").await.unwrap();
    let _: () = connection.sadd(&set_key, "member").await.unwrap();
    let _: () = connection.zadd(&zset_key, "member", 1).await.unwrap();
    let _: String = redis::cmd("XADD")
        .arg(&stream_key)
        .arg("*")
        .arg("field")
        .arg("value")
        .query_async(&mut connection)
        .await
        .unwrap();
    let app = configured(Some(&url), "admin-only");
    let response = app
        .clone()
        .oneshot(
            Request::get(format!("/v1/admin/redis?match={prefix}:*"))
                .header("authorization", "Bearer admin-only")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    let page: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(page["cursor"].is_string());
    assert_eq!(page["entries"].as_array().unwrap().len(), 7);
    for (key, expected_kind) in [
        (&string_key, "string"),
        (&large_key, "string"),
        (&list_key, "list"),
        (&hash_key, "hash"),
        (&set_key, "set"),
        (&zset_key, "zset"),
        (&stream_key, "stream"),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::get(format!("/v1/admin/redis?key={key}"))
                    .header("authorization", "Bearer admin-only")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{expected_kind}");
        let body = to_bytes(response.into_body(), 200_000).await.unwrap();
        let detail: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(detail["type"], expected_kind);
        if key == &large_key {
            assert_eq!(detail["value"].as_str().unwrap().len(), 65536);
            assert_eq!(detail["truncated"], true);
            assert_eq!(detail["size"], 100_000);
        }
    }
    let remaining: String = connection.get(&large_key).await.unwrap();
    assert_eq!(remaining.len(), 100_000);
    let _: () = connection
        .del(&[
            string_key, large_key, list_key, hash_key, set_key, zset_key, stream_key,
        ])
        .await
        .unwrap();
}
