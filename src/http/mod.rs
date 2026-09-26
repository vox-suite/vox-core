/**
* HTTP server endpoints, routing, and middleware assembly.
*/
pub mod admin;
pub mod agent_registry;
pub mod approvals;
pub mod audit;
pub mod auth;
pub mod capability_grants;
pub mod connected_reads;
pub mod connections;
pub mod consequential_writes;
pub mod conversations;
pub mod durable_tasks;
pub mod events;
pub mod execution;
pub mod execution_policy;
pub mod handoffs;
pub mod host_apps;
pub mod identity_adapters;
pub mod integration_registry;
pub mod preferences;
pub mod privacy;
pub mod rate_limit;
pub mod reminders;
pub mod remote_extensions;
pub mod schedules;
pub mod skills;
pub mod status;

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
    pub(crate) rate_limiter: rate_limit::RateLimiter,
    pub(crate) admin: Option<Arc<admin::RedisAdmin>>,
    pub(crate) audit: Option<Arc<crate::audit::AuditService>>,
    pub(crate) agent_registry: Option<Arc<crate::agent_registry::AgentRegistry>>,
    pub(crate) approvals: Option<Arc<crate::approvals::ApprovalService>>,
    pub(crate) db: Option<Db>,
    pub(crate) durable_tasks: Option<Arc<crate::durable_tasks::DurableTaskService>>,
    pub(crate) execution_policy: Option<Arc<crate::execution_policy::ExecutionPolicyService>>,
    pub(crate) execution: Option<Arc<crate::execution::ExecutionCoordinator>>,
    pub(crate) conversations: Option<Arc<ConversationService>>,
    pub(crate) connections: Option<Arc<crate::connections::ConnectionService>>,
    pub(crate) capability_grants: Option<Arc<crate::capability_grants::CapabilityGrantService>>,
    pub(crate) events: Option<Arc<EventService>>,
    pub(crate) host_trust: Option<Arc<HostTrustService>>,
    pub(crate) identity_adapters: Option<Arc<crate::identity_adapters::IdentityAdapterService>>,
    pub(crate) integration_registry: Option<Arc<crate::integration_registry::IntegrationRegistry>>,
    pub(crate) schedules: Option<Arc<ScheduleService>>,
    pub(crate) preferences: Option<Arc<crate::preferences::PreferenceService>>,
    pub(crate) privacy: Option<Arc<crate::privacy::PrivacyService>>,
    pub(crate) reminders: Option<Arc<crate::reminders::ReminderService>>,
    pub(crate) remote_extensions: Option<Arc<crate::remote_extensions::RemoteExtensionService>>,
    pub(crate) skills: Option<Arc<crate::skills::SkillService>>,
    pub(crate) status: Option<Arc<crate::status::StatusService>>,
    pub(crate) uber_read: Option<Arc<crate::providers::UberConnectedReadService>>,
    pub(crate) expedia_write: Option<Arc<crate::providers::ExpediaLodgingService>>,
    pub(crate) amazon: Option<Arc<crate::providers::AmazonService>>,
    pub(crate) zomato: Option<Arc<crate::providers::ZomatoService>>,
    pub(crate) service_token: Arc<str>,
}

impl AppState {
    pub fn new(ready: bool) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(ready)),
            rate_limiter: rate_limit::RateLimiter::new(rate_limit::RateLimitConfig::default()),
            admin: None,
            audit: None,
            agent_registry: None,
            approvals: None,
            db: None,
            durable_tasks: None,
            execution_policy: None,
            execution: None,
            conversations: None,
            connections: None,
            capability_grants: None,
            events: None,
            host_trust: None,
            identity_adapters: None,
            integration_registry: None,
            schedules: None,
            preferences: None,
            privacy: None,
            reminders: None,
            remote_extensions: None,
            skills: None,
            status: None,
            uber_read: None,
            expedia_write: None,
            amazon: None,
            zomato: None,
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
            rate_limiter: rate_limit::RateLimiter::new(rate_limit::RateLimitConfig::default()),
            admin: None,
            audit: Some(Arc::new(crate::audit::AuditService::new(db.clone()))),
            agent_registry: Some(Arc::new(crate::agent_registry::AgentRegistry::new(
                db.clone(),
            ))),
            approvals: Some(Arc::new(crate::approvals::ApprovalService::new(db.clone()))),
            db: Some(db.clone()),
            durable_tasks: Some(Arc::new(crate::durable_tasks::DurableTaskService::new(
                db.clone(),
            ))),
            execution_policy: Some(Arc::new(
                crate::execution_policy::ExecutionPolicyService::new(db.clone()),
            )),
            execution: Some(Arc::new(crate::execution::ExecutionCoordinator::new(
                db.clone(),
            ))),
            conversations: Some(Arc::new(conv)),
            connections: Some(Arc::new(crate::connections::ConnectionService::new(
                db.clone(),
            ))),
            capability_grants: Some(Arc::new(
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
            )),
            events: Some(Arc::new(EventService::new(db.clone()))),
            host_trust: Some(Arc::new(HostTrustService::new(db.clone()))),
            identity_adapters: Some(Arc::new(
                crate::identity_adapters::IdentityAdapterService::unavailable(db.clone()),
            )),
            integration_registry: Some(Arc::new(
                crate::integration_registry::IntegrationRegistry::new(db.clone()),
            )),
            schedules: Some(Arc::new(ScheduleService::new(db.clone()))),
            preferences: Some(Arc::new(crate::preferences::PreferenceService::new(
                db.clone(),
            ))),
            privacy: Some(Arc::new(crate::privacy::PrivacyService::new(
                db.clone(),
                None,
            ))),
            reminders: Some(Arc::new(crate::reminders::ReminderService::new(db.clone()))),
            remote_extensions: Some(Arc::new(
                crate::remote_extensions::RemoteExtensionService::new(db.clone()),
            )),
            skills: Some(Arc::new(crate::skills::SkillService::new(db.clone()))),
            status: Some(Arc::new(crate::status::StatusService::new(db.clone()))),
            uber_read: Some(Arc::new(crate::providers::UberConnectedReadService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultUberProviderClient::new(
                    "https://api.uber.com",
                )),
            ))),
            expedia_write: Some(Arc::new(crate::providers::ExpediaLodgingService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                crate::approvals::ApprovalService::new(db.clone()),
                crate::execution::ExecutionCoordinator::new(db.clone()),
                Arc::new(crate::providers::DefaultExpediaProviderClient::new(
                    "https://api.expediagroup.com",
                )),
            ))),
            amazon: Some(Arc::new(crate::providers::AmazonService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultAmazonProviderClient::new(
                    "https://webservices.amazon.com",
                )),
            ))),
            zomato: Some(Arc::new(crate::providers::ZomatoService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultZomatoProviderClient::new(
                    "https://api.zomato.com",
                )),
            ))),
            service_token: Arc::from(service_token),
        }
    }

    pub fn with_admin(mut self, admin: admin::RedisAdmin) -> Self {
        self.admin = Some(Arc::new(admin));
        self
    }

    pub fn with_rate_limiter(mut self, limiter: rate_limit::RateLimiter) -> Self {
        self.rate_limiter = limiter;
        self
    }

    pub fn service_token(&self) -> &str {
        &self.service_token
    }

    pub fn rate_limiter(&self) -> &rate_limit::RateLimiter {
        &self.rate_limiter
    }

    pub fn with_host_trust(db: Db, service_token: String) -> Self {
        Self {
            ready: Arc::new(AtomicBool::new(true)),
            rate_limiter: rate_limit::RateLimiter::new(rate_limit::RateLimitConfig::default()),
            admin: None,
            audit: Some(Arc::new(crate::audit::AuditService::new(db.clone()))),
            agent_registry: Some(Arc::new(crate::agent_registry::AgentRegistry::new(
                db.clone(),
            ))),
            approvals: Some(Arc::new(crate::approvals::ApprovalService::new(db.clone()))),
            db: Some(db.clone()),
            durable_tasks: Some(Arc::new(crate::durable_tasks::DurableTaskService::new(
                db.clone(),
            ))),
            execution_policy: Some(Arc::new(
                crate::execution_policy::ExecutionPolicyService::new(db.clone()),
            )),
            execution: Some(Arc::new(crate::execution::ExecutionCoordinator::new(
                db.clone(),
            ))),
            conversations: None,
            connections: Some(Arc::new(crate::connections::ConnectionService::new(
                db.clone(),
            ))),
            capability_grants: Some(Arc::new(
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
            )),
            events: None,
            host_trust: Some(Arc::new(HostTrustService::new(db.clone()))),
            identity_adapters: Some(Arc::new(
                crate::identity_adapters::IdentityAdapterService::unavailable(db.clone()),
            )),
            integration_registry: Some(Arc::new(
                crate::integration_registry::IntegrationRegistry::new(db.clone()),
            )),
            schedules: None,
            preferences: Some(Arc::new(crate::preferences::PreferenceService::new(
                db.clone(),
            ))),
            privacy: Some(Arc::new(crate::privacy::PrivacyService::new(
                db.clone(),
                None,
            ))),
            reminders: Some(Arc::new(crate::reminders::ReminderService::new(db.clone()))),
            remote_extensions: Some(Arc::new(
                crate::remote_extensions::RemoteExtensionService::new(db.clone()),
            )),
            skills: Some(Arc::new(crate::skills::SkillService::new(db.clone()))),
            status: Some(Arc::new(crate::status::StatusService::new(db.clone()))),
            uber_read: Some(Arc::new(crate::providers::UberConnectedReadService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultUberProviderClient::new(
                    "https://api.uber.com",
                )),
            ))),
            expedia_write: Some(Arc::new(crate::providers::ExpediaLodgingService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                crate::approvals::ApprovalService::new(db.clone()),
                crate::execution::ExecutionCoordinator::new(db.clone()),
                Arc::new(crate::providers::DefaultExpediaProviderClient::new(
                    "https://api.expediagroup.com",
                )),
            ))),
            amazon: Some(Arc::new(crate::providers::AmazonService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultAmazonProviderClient::new(
                    "https://webservices.amazon.com",
                )),
            ))),
            zomato: Some(Arc::new(crate::providers::ZomatoService::new(
                db.clone(),
                crate::connections::ConnectionService::new(db.clone()),
                crate::capability_grants::CapabilityGrantService::new(db.clone()),
                Arc::new(crate::providers::DefaultZomatoProviderClient::new(
                    "https://api.zomato.com",
                )),
            ))),
            service_token: Arc::from(service_token),
        }
    }

    pub fn with_uber_read(
        mut self,
        service: Arc<crate::providers::UberConnectedReadService>,
    ) -> Self {
        self.uber_read = Some(service);
        self
    }

    pub fn with_expedia_write(
        mut self,
        service: Arc<crate::providers::ExpediaLodgingService>,
    ) -> Self {
        self.expedia_write = Some(service);
        self
    }

    pub fn with_amazon(mut self, service: Arc<crate::providers::AmazonService>) -> Self {
        self.amazon = Some(service);
        self
    }

    pub fn with_zomato(mut self, service: Arc<crate::providers::ZomatoService>) -> Self {
        self.zomato = Some(service);
        self
    }

    pub fn with_reminders(mut self, service: Arc<crate::reminders::ReminderService>) -> Self {
        self.reminders = Some(service);
        self
    }

    pub fn take_host_trust(&mut self) -> Option<HostTrustService> {
        self.host_trust
            .take()
            .map(|trust| HostTrustService::clone(&trust))
    }

    pub fn set_host_trust(&mut self, trust: HostTrustService) {
        self.host_trust = Some(Arc::new(trust));
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

    pub fn with_status_secret_store(
        mut self,
        secrets: Arc<dyn crate::status::WebhookSecretStore>,
    ) -> Self {
        if let Some(db) = self.db.clone() {
            self.status = Some(Arc::new(
                crate::status::StatusService::new(db).with_secret_store(secrets),
            ));
        }
        self
    }

    pub fn with_agent_registry(
        mut self,
        agent_registry: crate::agent_registry::AgentRegistry,
    ) -> Self {
        self.agent_registry = Some(Arc::new(agent_registry));
        self
    }
}

pub fn router(state: AppState) -> Router {
    let rate_limiter = state.rate_limiter.clone();

    Router::new()
        .route(
            "/v1/admin/redis",
            get(admin::browse).delete(admin::delete).put(admin::update),
        )
        .route("/v1/admin/audit-events", get(audit::list))
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/conversations/respond", post(conversations::respond))
        .route("/v1/action-proposals", post(approvals::propose))
        .route(
            "/v1/spending-policies",
            post(execution_policy::set_spending_policy),
        )
        .route(
            "/v1/operational-quotas",
            post(execution_policy::set_operational_quota),
        )
        .route(
            "/v1/action-proposals/{id}/approve",
            post(approvals::approve),
        )
        .route("/v1/durable-tasks", post(durable_tasks::start))
        .route("/v1/durable-tasks/{id}", post(durable_tasks::get))
        .route("/v1/durable-tasks/{id}/wait", post(durable_tasks::wait))
        .route("/v1/durable-tasks/{id}/resume", post(durable_tasks::resume))
        .route("/v1/durable-tasks/{id}/cancel", post(durable_tasks::cancel))
        .route(
            "/v1/conversations/speculate",
            post(conversations::speculate),
        )
        .route(
            "/v1/conversations/respond/stream",
            post(conversations::respond_stream),
        )
        .route("/v1/conversations/complete", post(conversations::complete))
        .route("/v1/connections/initiate", post(connections::initiate))
        .route("/v1/connections/callback", post(connections::callback))
        .route("/v1/connections/authorize", post(connections::authorize))
        .route("/v1/connections/list", post(connections::list))
        .route(
            "/v1/connections/{id}/disconnect",
            post(connections::disconnect),
        )
        .route(
            "/v1/capability-grants",
            post(capability_grants::create).delete(capability_grants::revoke),
        )
        .route(
            "/v1/agents/{external_key}/effective-capability-grants",
            post(capability_grants::effective),
        )
        .route("/v1/events", post(events::ingest))
        .route("/v1/schedules", post(schedules::create))
        .route("/v1/schedules/{id}", patch(schedules::update))
        .route("/v1/reminders", post(reminders::create_reminder))
        .route("/v1/reminders/list", post(reminders::list_reminders))
        .route("/v1/reminders/{id}", post(reminders::get_reminder))
        .route(
            "/v1/reminders/{id}/cancel",
            post(reminders::cancel_reminder),
        )
        .route(
            "/v1/reminders/{id}/deliveries",
            post(reminders::get_reminder_deliveries),
        )
        .route(
            "/v1/reminders/{id}/delivery-callback",
            post(reminders::record_reminder_delivery),
        )
        .route("/v1/executions", post(execution::start))
        .route("/v1/executions/{id}", post(execution::get))
        .route("/v1/status-events", post(status::list))
        .route(
            "/v1/status-webhook-subscriptions",
            post(status::create_subscription).get(status::list_subscriptions),
        )
        .route(
            "/v1/status-webhook-subscriptions/{id}/rotate",
            post(status::rotate_subscription),
        )
        .route(
            "/v1/status-webhook-subscriptions/{id}",
            axum::routing::delete(status::disable_subscription),
        )
        .route("/v1/agent-definitions", post(agent_registry::register))
        .route(
            "/v1/agents/selected",
            post(agent_registry::list_selected_for_host),
        )
        .route("/v1/agent-selections", post(agent_registry::select))
        .route(
            "/v1/agent-definitions/enabled",
            post(agent_registry::set_enabled),
        )
        .route("/v1/integrations", post(integration_registry::register))
        .route(
            "/v1/integrations/enabled",
            post(integration_registry::set_enabled),
        )
        .route(
            "/v1/deployments/{external_key}/capabilities",
            get(integration_registry::discover),
        )
        .route(
            "/v1/deployments/{external_key}/integrations/{integration_key}/versions",
            get(integration_registry::versions),
        )
        .route(
            "/v1/capabilities/discover",
            post(integration_registry::discover_for_context),
        )
        .route(
            "/v1/deployments/{external_key}/agents",
            get(agent_registry::list_selected),
        )
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
            "/v1/preferences",
            post(preferences::set).get(preferences::list),
        )
        .route("/v1/preferences/{key}", delete(preferences::delete_key))
        .route(
            "/v1/agents/{agent_key}/effective-preferences",
            post(preferences::effective),
        )
        .route("/v1/privacy/delete-history", post(privacy::delete_history))
        .route(
            "/v1/privacy/portable-export",
            post(privacy::portable_export),
        )
        .route(
            "/v1/privacy/exports/{id}/download",
            post(privacy::download_export),
        )
        .route(
            "/v1/privacy/portable-import",
            post(privacy::portable_import),
        )
        .route(
            "/v1/privacy/retention-policy",
            get(privacy::get_retention_policy),
        )
        .route(
            "/v1/privacy/retention/prune",
            post(privacy::prune_retention),
        )
        .route(
            "/v1/privacy/executions/{id}/evidence",
            post(privacy::get_action_evidence),
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
        .route("/v1/remote-extensions", post(remote_extensions::install))
        .route("/v1/skills/private", post(skills::publish_private))
        .route("/v1/skills/curated", post(skills::publish_curated))
        .route("/v1/skills/list", post(skills::list))
        .route("/v1/skills/{id}/versions/{version}", post(skills::version))
        .route("/v1/skills/{id}/install", post(skills::install))
        .route("/v1/skills/{id}/disable", post(skills::disable))
        .route(
            "/v1/agents/{agent_key}/effective-skills",
            post(skills::effective),
        )
        .route(
            "/v1/agents/{agent_key}/skills/{skill_id}/load",
            post(skills::load_for_agent),
        )
        .route(
            "/v1/agents/{agent_key}/skills/{skill_id}/enable",
            post(skills::set_agent_enabled),
        )
        .route("/v1/remote-extensions/list", post(remote_extensions::list))
        .route(
            "/v1/remote-extensions/{id}",
            post(remote_extensions::get)
                .put(remote_extensions::update)
                .delete(remote_extensions::remove),
        )
        .route(
            "/v1/remote-extensions/{id}/enable",
            post(remote_extensions::set_enabled),
        )
        .route(
            "/v1/remote-extensions/{id}/conformance",
            post(remote_extensions::record_conformance),
        )
        .route(
            "/v1/remote-extensions/{id}/renew-consent",
            post(remote_extensions::renew_consent),
        )
        .route(
            "/v1/remote-extensions/{id}/quarantine",
            post(remote_extensions::quarantine),
        )
        .route(
            crate::host_trust::HOST_CONTEXT_PATH,
            post(host_apps::resolve_context),
        )
        .route("/v1/connected-reads", post(connected_reads::read))
        .route(
            "/v1/consequential-writes/propose",
            post(consequential_writes::propose),
        )
        .route(
            "/v1/consequential-writes/execute",
            post(consequential_writes::execute),
        )
        .route(
            "/v1/consequential-writes/cancel",
            post(consequential_writes::cancel),
        )
        .route(
            "/v1/consequential-writes/reconcile",
            post(consequential_writes::reconcile),
        )
        .route("/v1/handoffs/amazon", post(handoffs::amazon_handoff))
        .route("/v1/handoffs/zomato", post(handoffs::zomato_handoff))
        .route("/v1/handoffs/uber", post(handoffs::uber_handoff))
        .layer(axum::middleware::from_fn_with_state(
            rate_limiter,
            rate_limit::rate_limit_middleware,
        ))
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
