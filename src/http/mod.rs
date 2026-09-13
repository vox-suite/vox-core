pub mod auth;
pub mod conversations;

use crate::{
    agents::conversation::ConversationResponder, conversations::service::ConversationService,
    db::Db,
};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct AppState {
    ready: Arc<AtomicBool>,
    pub(crate) conversations: Option<Arc<ConversationService>>,
    pub(crate) service_token: Arc<str>,
}

impl AppState {
    pub fn new(ready: bool) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(ready)),
            conversations: None,
            service_token: Arc::from(""),
        }
    }

    pub fn with_dependencies(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        service_token: String,
    ) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            conversations: Some(Arc::new(ConversationService::new(db, agent))),
            service_token: Arc::from(service_token),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/conversations/respond", post(conversations::respond))
        .with_state(state)
}

async fn live() -> StatusCode {
    StatusCode::OK
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    if state.ready.load(Ordering::Acquire) {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    }
}
