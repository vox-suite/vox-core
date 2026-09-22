/**
* Integration tests for system health checks and liveness probes.
*/
use axum::{body::Body, http::Request};
use tower::ServiceExt;
use vox_core::http::{AppState, router};

#[tokio::test]
async fn reports_liveness_and_dependency_readiness_separately() {
    let state = AppState::new(false);
    let app = router(state.clone());

    let live = app
        .clone()
        .oneshot(Request::get("/health/live").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(live.status(), 200);

    let unavailable = app
        .clone()
        .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(unavailable.status(), 503);

    state.set_ready(true);
    let ready = app
        .oneshot(Request::get("/health/ready").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(ready.status(), 200);
}
