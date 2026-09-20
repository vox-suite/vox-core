pub mod actions;
pub mod admin;
pub mod auth;
pub mod conversations;
pub mod events;
pub mod host_apps;
pub mod identity_adapters;
pub mod schedules;

use crate::{
    agents::conversation::ConversationResponder, conversations::service::ConversationService,
    db::Db, events::service::EventService, host_trust::HostTrustService, memory::MemoryService,
    schedules::service::ScheduleService,
};
use axum::{
    Router,
    extract::State,
    http::StatusCode,
    routing::{delete, get, patch, post},
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
    pub(crate) host_trust: Option<Arc<HostTrustService>>,
    pub(crate) identity_adapters: Option<Arc<crate::identity_adapters::IdentityAdapterService>>,
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
            host_trust: None,
            identity_adapters: None,
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
        Self::with_memory_and_jev(db, agent, memory, service_token, None)
    }

    pub fn with_memory_and_jev(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        memory: MemoryService,
        service_token: String,
        jev: Option<crate::jev::JevClient>,
    ) -> Self {
        let mut conv = ConversationService::with_memory(db.clone(), agent, memory);
        if let Some(j) = jev {
            conv = conv.with_jev(j);
        }
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            admin: None,
            db: Some(db.clone()),
            conversations: Some(Arc::new(conv)),
            events: Some(Arc::new(EventService::new(db.clone()))),
            host_trust: Some(Arc::new(HostTrustService::new(db.clone()))),
            identity_adapters: Some(Arc::new(
                crate::identity_adapters::IdentityAdapterService::unavailable(db.clone()),
            )),
            schedules: Some(Arc::new(ScheduleService::new(db))),
            service_token: Arc::from(service_token),
        }
    }

    pub fn with_admin(mut self, admin: admin::RedisAdmin) -> Self {
        self.admin = Some(Arc::new(admin));
        self
    }

    pub fn with_host_trust(db: Db, service_token: String) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            admin: None,
            db: Some(db.clone()),
            conversations: None,
            events: None,
            host_trust: Some(Arc::new(HostTrustService::new(db.clone()))),
            identity_adapters: Some(Arc::new(
                crate::identity_adapters::IdentityAdapterService::unavailable(db),
            )),
            schedules: None,
            service_token: Arc::from(service_token),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::Release);
    }

    pub fn with_identity_adapters(
        mut self,
        identity_adapters: crate::identity_adapters::IdentityAdapterService,
    ) -> Self {
        self.identity_adapters = Some(Arc::new(identity_adapters));
        self
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/admin/redis", get(admin::browse))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/conversations/respond", post(conversations::respond))
        .route(
            "/v1/conversations/respond/stream",
            post(conversations::respond_stream),
        )
        .route("/v1/conversations/complete", post(conversations::complete))
        .route("/v1/events", post(events::ingest))
        .route("/v1/schedules", post(schedules::create))
        .route("/v1/schedules/{id}", patch(schedules::update))
        .route("/v1/actions/{id}/result", post(actions::record_result))
        .route("/v1/host-apps", post(host_apps::register))
        .route("/v1/identity-adapters", post(identity_adapters::register))
        .route(
            "/v1/identity/passwordless/challenges",
            post(identity_adapters::start_passwordless_recovery),
        )
        .route(
            "/v1/identity/authentications",
            post(identity_adapters::authenticate),
        )
        .route(
            "/v1/identity/links",
            post(identity_adapters::link).delete(identity_adapters::unlink),
        )
        .route(
            "/v1/host-apps/{id}/credentials",
            post(host_apps::rotate_credential),
        )
        .route(
            "/v1/host-app-credentials/{id}",
            delete(host_apps::revoke_credential),
        )
        .route(
            crate::host_trust::HOST_CONTEXT_PATH,
            post(host_apps::resolve_context),
        )
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
