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
        context: &ResolvedUserContext,
        input: AppendStatusEvent,
    ) -> Result<StatusEvent, StatusError> {
        let aggregate_type = trimmed(&input.aggregate_type, 64).ok_or(StatusError::Invalid)?;
        let event_type = trimmed(&input.event_type, 128).ok_or(StatusError::Invalid)?;
        let state = trimmed(&input.state, 128).ok_or(StatusError::Invalid)?;
        let key = trimmed(&input.deduplication_key, 255).ok_or(StatusError::Invalid)?;
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(73125, hashtext($1::text))")
            .bind(context.id.0)
            .execute(&mut *tx)
            .await?;
        let existing = sqlx::query(
            "SELECT cursor, aggregate_type, aggregate_id, event_type, state, occurred_at, committed_at, payload
             FROM status_events WHERE user_context_id=$1 AND deduplication_key=$2",
        )
        .bind(context.id.0)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        if let Some(row) = existing {
            tx.commit().await?;
            return Ok(event_from_row(&row));
        }
        let row = sqlx::query(
            "INSERT INTO status_events (user_context_id, aggregate_type, aggregate_id, event_type, state, deduplication_key, occurred_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7)
             RETURNING cursor, aggregate_type, aggregate_id, event_type, state, occurred_at, committed_at, payload",
        )
        .bind(context.id.0)
        .bind(aggregate_type)
        .bind(input.aggregate_id)
        .bind(event_type)
        .bind(state)
        .bind(key)
        .bind(input.occurred_at)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(event_from_row(&row))
    }

    pub async fn list(
        &self,
        context: &ResolvedUserContext,
        after: i64,
        limit: i64,
    ) -> Result<Vec<StatusEvent>, StatusError> {
        if after < 0 || !(1..=200).contains(&limit) {
            return Err(StatusError::Invalid);
        }
        let rows = sqlx::query(
            "SELECT cursor, aggregate_type, aggregate_id, event_type, state, occurred_at, committed_at, payload
             FROM status_events WHERE user_context_id=$1 AND cursor>$2 ORDER BY cursor LIMIT $3",
        )
        .bind(context.id.0)
        .bind(after)
        .bind(limit)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows.iter().map(event_from_row).collect())
    }

    pub async fn create_subscription(
        &self,
        context: &ResolvedUserContext,
        request: CreateSubscriptionRequest,
    ) -> Result<WebhookSubscription, StatusError> {
        let endpoint = webhook_endpoint(&request.endpoint)?;
        let id = Uuid::new_v4();
        let secret = new_webhook_secret();
        self.secrets.put(id, secret.clone()).await?;
        if let Err(error) = sqlx::query(
            "INSERT INTO status_webhook_subscriptions (id,user_context_id,endpoint) VALUES ($1,$2,$3)",
        )
        .bind(id)
        .bind(context.id.0)
        .bind(&endpoint)
        .execute(self.db.pool())
        .await
        {
            let _ = self.secrets.delete(id).await;
            return Err(error.into());
        }
        Ok(WebhookSubscription {
            id,
            endpoint,
            state: "enabled".into(),
            secret: Some(secret),
        })
    }

    pub async fn rotate_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<WebhookSubscription, StatusError> {
        let owned = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM status_webhook_subscriptions WHERE id=$1 AND user_context_id=$2 AND state='enabled'",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?;
        if owned.is_none() {
            return Err(StatusError::NotFound);
        }
        let old_secret = self.secrets.get(id).await?;
        let secret = new_webhook_secret();
        self.secrets.put(id, secret.clone()).await?;
        let updated = sqlx::query(
            "UPDATE status_webhook_subscriptions SET secret_version=secret_version+1, updated_at=now()
             WHERE id=$1 AND user_context_id=$2 AND state='enabled' RETURNING endpoint",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await;
        match updated {
            Ok(Some(row)) => Ok(WebhookSubscription {
                id,
                endpoint: row.get("endpoint"),
                state: "enabled".into(),
                secret: Some(secret),
            }),
            other => {
                self.secrets.put(id, old_secret).await?;
                match other {
                    Ok(None) => Err(StatusError::NotFound),
                    Err(error) => Err(error.into()),
                    _ => unreachable!(),
                }
            }
        }
    }

    pub async fn disable_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<(), StatusError> {
        let changed = sqlx::query(
            "UPDATE status_webhook_subscriptions SET state='disabled', updated_at=now()
             WHERE id=$1 AND user_context_id=$2 AND state='enabled'",
        )
        .bind(id)
        .bind(context.id.0)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StatusError::NotFound);
        }
        self.secrets.delete(id).await?;
        Ok(())
    }

    pub async fn list_subscriptions(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<WebhookSubscription>, StatusError> {
        let rows = sqlx::query(
            "SELECT id, endpoint, state FROM status_webhook_subscriptions
             WHERE user_context_id=$1 ORDER BY created_at, id",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|row| WebhookSubscription {
                id: row.get("id"),
                endpoint: row.get("endpoint"),
                state: row.get("state"),
                secret: None,
            })
            .collect())
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

fn event_from_row(row: &sqlx::postgres::PgRow) -> StatusEvent {
    StatusEvent {
        cursor: row.get("cursor"),
        aggregate_type: row.get("aggregate_type"),
        aggregate_id: row.get("aggregate_id"),
        event_type: row.get("event_type"),
        state: row.get("state"),
        occurred_at: row.get("occurred_at"),
        committed_at: row.get("committed_at"),
        payload: row.get("payload"),
        authoritative: false,
    }
}

fn new_webhook_secret() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
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
