use super::*;
use axum::{
    Json, Router,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use serde_json::{Value, json};

async fn server(status: StatusCode) -> String {
    let app = Router::new().route("/search", post(move |headers: HeaderMap, Json(body): Json<Value>| async move {
        assert_eq!(headers["x-api-key"], "test-key");
        assert_eq!(body["query"], "latest Rust release");
        assert!(body["numResults"].as_u64().unwrap() <= 5);
        (status, Json(json!({"results": [{"title": "Rust", "url": "https://rust-lang.org", "highlights": ["Release news"]}]})))
    }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/search", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
async fn returns_sources_and_content() {
    let tool = WebSearch {
        client: reqwest::Client::new(),
        api_key: "test-key".into(),
        endpoint: server(StatusCode::OK).await,
    };
    let output = tool
        .call(SearchArgs {
            query: "latest Rust release".into(),
        })
        .await
        .unwrap();
    assert_eq!(output["results"][0]["url"], "https://rust-lang.org");
    assert_eq!(output["results"][0]["highlights"][0], "Release news");
}

#[tokio::test]
async fn rejects_provider_errors_and_empty_queries() {
    let tool = WebSearch {
        client: reqwest::Client::new(),
        api_key: "test-key".into(),
        endpoint: server(StatusCode::UNAUTHORIZED).await,
    };
    assert!(
        tool.call(SearchArgs {
            query: "latest Rust release".into()
        })
        .await
        .is_err()
    );
    assert!(tool.call(SearchArgs { query: "  ".into() }).await.is_err());
}
