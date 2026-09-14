pub mod actions;
pub mod admin;
pub mod auth;
pub mod conversations;
pub mod events;
pub mod schedules;

use crate::{
    agents::conversation::ConversationResponder, conversations::service::ConversationService,
    db::Db, events::service::EventService, memory::MemoryService,
    schedules::service::ScheduleService,
};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{get, patch, post},
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone)]
pub struct AppState {
    ready: Arc<AtomicBool>,
    pub(crate) admin: Option<Arc<admin::RedisAdmin>>,
    pub(crate) db: Option<Db>,
    pub(crate) conversations: Option<Arc<ConversationService>>,
    pub(crate) events: Option<Arc<EventService>>,
    pub(crate) schedules: Option<Arc<ScheduleService>>,
    pub(crate) service_token: Arc<str>,
}

impl AppState {
    pub fn new(ready: bool) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(ready)),
            admin: None,
            db: None,
            conversations: None,
            events: None,
            schedules: None,
            service_token: Arc::from(""),
        }
    }

    pub fn with_dependencies(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        service_token: String,
    ) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, agent, memory, service_token)
    }

    pub fn with_memory(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        memory: MemoryService,
        service_token: String,
    ) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            admin: None,
            db: Some(db.clone()),
            conversations: Some(Arc::new(ConversationService::with_memory(
                db.clone(),
                agent,
                memory,
            ))),
            events: Some(Arc::new(EventService::new(db.clone()))),
            schedules: Some(Arc::new(ScheduleService::new(db))),
            service_token: Arc::from(service_token),
        }
    }

    pub fn with_admin(mut self, admin: admin::RedisAdmin) -> Self {
        self.admin = Some(Arc::new(admin));
        self
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/admin/redis", get(admin::browse))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/conversations/respond", post(conversations::respond))
        .route("/v1/conversations/complete", post(conversations::complete))
        .route("/v1/events", post(events::ingest))
        .route("/v1/schedules", post(schedules::create))
        .route("/v1/schedules/{id}", patch(schedules::update))
        .route("/v1/actions/{id}/result", post(actions::record_result))
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
