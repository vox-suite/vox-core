use crate::{
    db::Db,
    execution::{AdapterOutcome, Execution, ExecutionCoordinator, ExecutionError},
    identity::{
        DeploymentId, HostAppId, HostOrganizationId, ResolvedUserContext, UserContextId,
        UserContextSubject, UserId,
    },
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct StatusEvent {
    pub cursor: i64,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub event_type: String,
    pub state: String,
    pub occurred_at: DateTime<Utc>,
    pub committed_at: DateTime<Utc>,
    pub payload: Value,
    pub authoritative: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct CreateSubscriptionRequest {
    pub endpoint: String,
}

#[derive(Clone, Debug)]
pub struct AppendStatusEvent {
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub event_type: String,
    pub state: String,
    pub deduplication_key: String,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct WebhookSubscription {
    pub id: Uuid,
    pub endpoint: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[async_trait::async_trait]
pub trait WebhookSecretStore: Send + Sync {
    async fn put(&self, subscription_id: Uuid, secret: String) -> Result<(), StatusError>;
    async fn get(&self, subscription_id: Uuid) -> Result<String, StatusError>;
    async fn delete(&self, subscription_id: Uuid) -> Result<(), StatusError>;
}

#[derive(Clone)]
pub struct UnavailableWebhookSecretStore;

#[async_trait::async_trait]
impl WebhookSecretStore for UnavailableWebhookSecretStore {
    async fn put(&self, _: Uuid, _: String) -> Result<(), StatusError> {
        Err(StatusError::Unavailable)
    }
    async fn get(&self, _: Uuid) -> Result<String, StatusError> {
        Err(StatusError::Unavailable)
    }
    async fn delete(&self, _: Uuid) -> Result<(), StatusError> {
        Err(StatusError::Unavailable)
    }
}

#[derive(Clone, Debug)]
pub struct VerifiedIntegrationEvent {
    pub integration_external_key: String,
    pub execution_id: Uuid,
    pub provider_event_id: String,
    pub external_account_reference: String,
    pub outcome: AdapterOutcome,
}

#[async_trait::async_trait]
pub trait IntegrationExternalEventVerifier: Send + Sync {
    async fn verify(&self, raw: &[u8]) -> Result<VerifiedIntegrationEvent, StatusError>;
}

#[derive(Clone)]
pub struct StatusService {
    db: Db,
    secrets: Arc<dyn WebhookSecretStore>,
}

#[derive(Debug, thiserror::Error)]
pub enum StatusError {
    #[error("status request invalid")]
    Invalid,
    #[error("status record unavailable")]
    NotFound,
    #[error("webhook secret custody unavailable")]
    Unavailable,
    #[error("status storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("execution update unavailable")]
    Execution(#[from] ExecutionError),
}

impl StatusService {
    pub fn new(db: Db) -> Self {
        Self {
            db,
            secrets: Arc::new(UnavailableWebhookSecretStore),
        }
    }

    pub fn with_secret_store(mut self, secrets: Arc<dyn WebhookSecretStore>) -> Self {
        self.secrets = secrets;
        self
    }

    pub fn delivery_worker(&self) -> WebhookDeliveryWorker {
        WebhookDeliveryWorker::new(self.db.clone(), Arc::clone(&self.secrets))
    }

    pub async fn append(
        &self,
        _context: &ResolvedUserContext,
        _input: AppendStatusEvent,
    ) -> Result<StatusEvent, StatusError> {
        Err(StatusError::Unavailable)
    }

    pub async fn list(
        &self,
        _context: &ResolvedUserContext,
        _after: i64,
        _limit: i64,
    ) -> Result<Vec<StatusEvent>, StatusError> {
        Ok(vec![])
    }

    pub async fn create_subscription(
        &self,
        _context: &ResolvedUserContext,
        _request: CreateSubscriptionRequest,
    ) -> Result<WebhookSubscription, StatusError> {
        Err(StatusError::Unavailable)
    }

    pub async fn rotate_subscription(
        &self,
        _context: &ResolvedUserContext,
        _id: Uuid,
    ) -> Result<WebhookSubscription, StatusError> {
        Err(StatusError::Unavailable)
    }

    pub async fn disable_subscription(
        &self,
        _context: &ResolvedUserContext,
        _id: Uuid,
    ) -> Result<(), StatusError> {
        Err(StatusError::Unavailable)
    }

    pub async fn list_subscriptions(
        &self,
        _context: &ResolvedUserContext,
    ) -> Result<Vec<WebhookSubscription>, StatusError> {
        Ok(vec![])
    }

    pub async fn apply_verified_external_event(
        &self,
        coordinator: &ExecutionCoordinator,
        incoming: VerifiedIntegrationEvent,
        now: DateTime<Utc>,
    ) -> Result<Execution, StatusError> {
        let provider_key =
            trimmed(&incoming.integration_external_key, 255).ok_or(StatusError::Invalid)?;
        let _event_id = trimmed(&incoming.provider_event_id, 255).ok_or(StatusError::Invalid)?;
        let account_hash = hex::encode(Sha256::digest(
            trimmed(&incoming.external_account_reference, 512).ok_or(StatusError::Invalid)?,
        ));
        let row = sqlx::query(
            "SELECT e.user_id, e.provider_reference, uc.id AS context_id, \
                    uc.deployment_id, uc.host_app_id, uc.organization_id, uc.host_user_id \
             FROM executions e \
             JOIN connections c ON c.id = e.connection_id \
               AND c.user_id = e.user_id AND c.user_context_id = e.user_context_id \
             JOIN user_contexts uc ON uc.id = e.user_context_id AND uc.user_id = e.user_id \
             WHERE e.id = $1 AND c.provider_key = $2 AND c.external_account_hash = $3",
        )
        .bind(incoming.execution_id)
        .bind(provider_key)
        .bind(account_hash)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(StatusError::NotFound)?;
        let bound_reference: Option<String> = row.get("provider_reference");
        if bound_reference.as_deref() != outcome_provider_reference(&incoming.outcome) {
            return Err(StatusError::NotFound);
        }
        let user_id: Uuid = row.get("user_id");
        let context = ResolvedUserContext {
            id: UserContextId(row.get("context_id")),
            user_id: UserId(user_id),
            subject: UserContextSubject {
                deployment_id: DeploymentId(row.get("deployment_id")),
                host_app_id: HostAppId(row.get("host_app_id")),
                organization_id: row
                    .get::<Option<Uuid>, _>("organization_id")
                    .map(HostOrganizationId),
                host_user_id: row.get("host_user_id"),
            },
        };
        coordinator
            .record_verified_external_outcome(
                &context,
                incoming.execution_id,
                incoming.outcome,
                now,
            )
            .await
            .map_err(StatusError::from)
    }
}

#[allow(dead_code)]
pub struct WebhookDeliveryWorker {
    db: Db,
    secrets: Arc<dyn WebhookSecretStore>,
    client: reqwest::Client,
}

impl WebhookDeliveryWorker {
    pub fn new(db: Db, secrets: Arc<dyn WebhookSecretStore>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .expect("static webhook client configuration is valid");
        Self {
            db,
            secrets,
            client,
        }
    }

    pub async fn deliver_next(
        &self,
        _worker: &str,
        _now: DateTime<Utc>,
    ) -> Result<bool, StatusError> {
        Ok(false)
    }
}

fn trimmed(value: &str, max: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max).then_some(value)
}

fn outcome_provider_reference(outcome: &AdapterOutcome) -> Option<&str> {
    match outcome {
        AdapterOutcome::Succeeded {
            provider_reference, ..
        } => Some(provider_reference),
        AdapterOutcome::Cancelled {
            provider_reference, ..
        }
        | AdapterOutcome::AwaitingProviderAuthentication { provider_reference }
        | AdapterOutcome::Reconciling { provider_reference }
        | AdapterOutcome::Unknown {
            provider_reference, ..
        } => provider_reference.as_deref(),
        AdapterOutcome::Failed { .. } => None,
    }
}

#[allow(dead_code)]
fn webhook_endpoint(value: &str) -> Result<String, StatusError> {
    let parsed = Url::parse(value.trim()).map_err(|_| StatusError::Invalid)?;
    if parsed.scheme() != "https"
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.host_str().is_none_or(|host| {
            host.eq_ignore_ascii_case("localhost")
                || host.parse::<IpAddr>().is_ok_and(|ip| match ip {
                    IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
                    IpAddr::V6(v6) => v6.is_loopback() || v6.is_unique_local(),
                })
        })
    {
        return Err(StatusError::Invalid);
    }
    Ok(parsed.to_string())
}

#[allow(dead_code)]
fn webhook_signature(secret: &str, timestamp: &str, body: &[u8]) -> Result<String, StatusError> {
    use hmac::{Hmac, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| StatusError::Invalid)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
#[path = "../../tests/unit/status.rs"]
mod tests;
