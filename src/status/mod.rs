/**
* Service health status, metrics, and diagnostics.
*/
use crate::{
    db::Db,
    execution::{AdapterOutcome, Execution, ExecutionCoordinator, ExecutionError},
    identity::ResolvedUserContext,
};
use chrono::{DateTime, Utc};
use hmac::Mac;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::Row;
use std::{net::IpAddr, sync::Arc, time::Duration};
use url::Url;
use uuid::Uuid;

const MAX_DELIVERY_ATTEMPTS: i32 = 8;

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
        if !matches!(input.aggregate_type.as_str(), "task" | "run" | "execution")
            || trimmed(&input.deduplication_key, 255).is_none()
            || trimmed(&input.event_type, 255).is_none()
            || trimmed(&input.state, 255).is_none()
        {
            return Err(StatusError::Invalid);
        }
        let row = sqlx::query("INSERT INTO status_events (user_context_id,aggregate_type,aggregate_id,event_type,state,occurred_at,deduplication_key) VALUES ($1,$2,$3,$4,$5,$6,$7) ON CONFLICT (user_context_id,deduplication_key) DO UPDATE SET deduplication_key=EXCLUDED.deduplication_key RETURNING cursor,aggregate_type,aggregate_id,event_type,state,occurred_at,committed_at,payload")
            .bind(context.id.0).bind(input.aggregate_type).bind(input.aggregate_id).bind(input.event_type).bind(input.state).bind(input.occurred_at).bind(input.deduplication_key)
            .fetch_one(self.db.pool()).await?;
        event(row)
    }

    pub async fn list(
        &self,
        context: &ResolvedUserContext,
        after: i64,
        limit: i64,
    ) -> Result<Vec<StatusEvent>, StatusError> {
        if after < 0 || !(1..=100).contains(&limit) {
            return Err(StatusError::Invalid);
        }
        let rows = sqlx::query("SELECT cursor,aggregate_type,aggregate_id,event_type,state,occurred_at,committed_at,payload FROM status_events WHERE user_context_id=$1 AND cursor>$2 ORDER BY cursor LIMIT $3")
            .bind(context.id.0).bind(after).bind(limit).fetch_all(self.db.pool()).await?;
        rows.into_iter().map(event).collect()
    }

    pub async fn create_subscription(
        &self,
        context: &ResolvedUserContext,
        request: CreateSubscriptionRequest,
    ) -> Result<WebhookSubscription, StatusError> {
        let endpoint = webhook_endpoint(&request.endpoint)?;
        let secret = new_secret();
        let row = sqlx::query("INSERT INTO status_webhook_subscriptions (user_context_id,endpoint,secret_hash) VALUES ($1,$2,$3) RETURNING id,endpoint,state")
            .bind(context.id.0).bind(endpoint.as_str()).bind(secret_hash(&secret)).fetch_one(self.db.pool()).await?;
        let id: Uuid = row.get("id");
        if self.secrets.put(id, secret.clone()).await.is_err() {
            sqlx::query("DELETE FROM status_webhook_subscriptions WHERE id=$1")
                .bind(id)
                .execute(self.db.pool())
                .await?;
            return Err(StatusError::Unavailable);
        }
        Ok(WebhookSubscription {
            id,
            endpoint: row.get("endpoint"),
            state: row.get("state"),
            secret: Some(secret),
        })
    }

    pub async fn rotate_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<WebhookSubscription, StatusError> {

        self.subscription(context, id, None).await?;
        let previous_secret = self.secrets.get(id).await?;
        let secret = new_secret();
        self.secrets.put(id, secret.clone()).await?;
        let changed = sqlx::query("UPDATE status_webhook_subscriptions SET secret_hash=$3,state='enabled',updated_at=now() WHERE id=$1 AND user_context_id=$2")
            .bind(id).bind(context.id.0).bind(secret_hash(&secret)).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            let _ = self.secrets.put(id, previous_secret).await;
            return Err(StatusError::NotFound);
        }
        self.subscription(context, id, Some(secret)).await
    }

    pub async fn disable_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<(), StatusError> {
        let changed = sqlx::query("UPDATE status_webhook_subscriptions SET state='disabled',updated_at=now() WHERE id=$1 AND user_context_id=$2")
            .bind(id).bind(context.id.0).execute(self.db.pool()).await?.rows_affected();
        if changed != 1 {
            return Err(StatusError::NotFound);
        }
        Ok(())
    }

    pub async fn list_subscriptions(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<WebhookSubscription>, StatusError> {
        let rows = sqlx::query("SELECT id,endpoint,state FROM status_webhook_subscriptions WHERE user_context_id=$1 ORDER BY created_at")
            .bind(context.id.0).fetch_all(self.db.pool()).await?;
        Ok(rows
            .into_iter()
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
        let integration =
            trimmed(&incoming.integration_external_key, 255).ok_or(StatusError::Invalid)?;
        let event_id = trimmed(&incoming.provider_event_id, 255).ok_or(StatusError::Invalid)?;
        let mut tx = self.db.pool().begin().await?;
        let account_hash = Sha256::digest(
            trimmed(&incoming.external_account_reference, 512).ok_or(StatusError::Invalid)?,
        )
        .to_vec();
        let row = sqlx::query("SELECT uc.id,uc.user_id,uc.deployment_id,uc.host_app_id,uc.organization_id,uc.host_user_id,e.provider_reference FROM executions e JOIN user_contexts uc ON uc.id=e.user_context_id JOIN external_connections c ON c.id=e.connection_id JOIN integration_definitions i ON i.id=c.integration_id WHERE e.id=$1 AND e.integration_external_key=$2 AND i.external_key=$2 AND i.state='enabled' AND c.user_context_id=e.user_context_id AND c.external_account_hash=$3")
            .bind(incoming.execution_id).bind(integration).bind(account_hash).fetch_optional(&mut *tx).await?.ok_or(StatusError::NotFound)?;
        let bound_reference: Option<String> = row.get("provider_reference");
        if bound_reference.as_deref() != outcome_provider_reference(&incoming.outcome) {
            return Err(StatusError::NotFound);
        }
        let event_row = sqlx::query("INSERT INTO integration_external_events (execution_id,integration_external_key,provider_event_id) VALUES ($1,$2,$3) ON CONFLICT (integration_external_key,provider_event_id) DO NOTHING RETURNING id")
            .bind(incoming.execution_id).bind(integration).bind(event_id).fetch_optional(&mut *tx).await?;
        let context = context_from_row(row)?;
        if event_row.is_none() {
            tx.commit().await?;
            return coordinator
                .get(&context, incoming.execution_id)
                .await
                .map_err(StatusError::from);
        }
        let execution = coordinator
            .record_verified_external_outcome_in_transaction(
                &mut tx,
                &context,
                incoming.execution_id,
                incoming.outcome,
                now,
            )
            .await?;
        tx.commit().await?;
        Ok(execution)
    }

    async fn subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
        secret: Option<String>,
    ) -> Result<WebhookSubscription, StatusError> {
        let row = sqlx::query("SELECT id,endpoint,state FROM status_webhook_subscriptions WHERE id=$1 AND user_context_id=$2")
            .bind(id).bind(context.id.0).fetch_optional(self.db.pool()).await?.ok_or(StatusError::NotFound)?;
        Ok(WebhookSubscription {
            id: row.get("id"),
            endpoint: row.get("endpoint"),
            state: row.get("state"),
            secret,
        })
    }
}

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
        worker: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StatusError> {
        if trimmed(worker, 255).is_none() {
            return Err(StatusError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query("WITH candidate AS (SELECT id FROM status_webhook_deliveries WHERE (state='pending' AND next_attempt_at <= $1) OR (state='leased' AND lease_expires_at <= $1) ORDER BY next_attempt_at FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE status_webhook_deliveries d SET state='leased',lease_owner=$2,lease_expires_at=$1 + interval '30 seconds',attempts=attempts+1,updated_at=$1 FROM candidate WHERE d.id=candidate.id RETURNING d.id,d.subscription_id,d.status_cursor,d.attempts")
            .bind(now).bind(worker).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        tx.commit().await?;
        let delivery_id: Uuid = row.get("id");
        let subscription_id: Uuid = row.get("subscription_id");
        let cursor: i64 = row.get("status_cursor");
        let attempts: i32 = row.get("attempts");
        let secret = match self.secrets.get(subscription_id).await {
            Ok(value) => value,
            Err(_) => {
                self.retry_or_fail(delivery_id, attempts, now, false)
                    .await?;
                return Ok(true);
            }
        };
        let data = sqlx::query("SELECT s.endpoint,e.aggregate_type,e.aggregate_id,e.event_type,e.state FROM status_webhook_subscriptions s JOIN status_webhook_deliveries d ON d.subscription_id=s.id JOIN status_events e ON e.cursor=d.status_cursor WHERE d.id=$1 AND s.state='enabled'")
            .bind(delivery_id).fetch_optional(self.db.pool()).await?;
        let Some(data) = data else {
            self.retry_or_fail(delivery_id, attempts, now, false)
                .await?;
            return Ok(true);
        };
        let payload = serde_json::json!({"version":1,"delivery_id":delivery_id,"cursor":cursor,"authoritative":false,"fetch_authoritative_state":true,"aggregate":{"type":data.get::<String,_>("aggregate_type"),"id":data.get::<Uuid,_>("aggregate_id")},"event_type":data.get::<String,_>("event_type"),"state":data.get::<String,_>("state")});
        let body = serde_json::to_vec(&payload).map_err(|_| StatusError::Invalid)?;
        let timestamp = now.timestamp().to_string();
        let signature = webhook_signature(&secret, &timestamp, &body)?;
        let delivered = self
            .client
            .post(data.get::<String, _>("endpoint"))
            .header("x-vox-signature-version", "v1")
            .header("x-vox-signature", signature)
            .header("x-vox-timestamp", timestamp)
            .header("x-vox-delivery-id", delivery_id.to_string())
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .map(|response| response.status().is_success())
            .unwrap_or(false);
        if delivered {
            sqlx::query("UPDATE status_webhook_deliveries SET state='delivered',delivered_at=$1,lease_owner=NULL,lease_expires_at=NULL,updated_at=$1 WHERE id=$2").bind(now).bind(delivery_id).execute(self.db.pool()).await?;
        } else {
            self.retry_or_fail(delivery_id, attempts, now, false)
                .await?;
        }
        Ok(true)
    }

    async fn retry_or_fail(
        &self,
        delivery_id: Uuid,
        attempts: i32,
        now: DateTime<Utc>,
        secret_failure: bool,
    ) -> Result<(), StatusError> {
        let permanently_failed = attempts >= MAX_DELIVERY_ATTEMPTS || secret_failure;
        let mut tx = self.db.pool().begin().await?;
        let subscription = sqlx::query_scalar::<_,Uuid>("UPDATE status_webhook_deliveries SET state=CASE WHEN $1 THEN 'failed' ELSE 'pending' END,next_attempt_at=$2 + make_interval(secs => LEAST(3600, 2 ^ LEAST(attempts, 12))::int),lease_owner=NULL,lease_expires_at=NULL,updated_at=$2 WHERE id=$3 RETURNING subscription_id")
            .bind(permanently_failed).bind(now).bind(delivery_id).fetch_one(&mut *tx).await?;
        if permanently_failed {
            sqlx::query("UPDATE status_webhook_subscriptions SET state='unhealthy',updated_at=$1 WHERE id=$2 AND state='enabled'").bind(now).bind(subscription).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

fn event(row: sqlx::postgres::PgRow) -> Result<StatusEvent, StatusError> {
    Ok(StatusEvent {
        cursor: row.try_get("cursor")?,
        aggregate_type: row.try_get("aggregate_type")?,
        aggregate_id: row.try_get("aggregate_id")?,
        event_type: row.try_get("event_type")?,
        state: row.try_get("state")?,
        occurred_at: row.try_get("occurred_at")?,
        committed_at: row.try_get("committed_at")?,
        payload: row.try_get("payload")?,
        authoritative: false,
    })
}

fn context_from_row(row: sqlx::postgres::PgRow) -> Result<ResolvedUserContext, StatusError> {
    Ok(ResolvedUserContext {
        id: crate::identity::UserContextId(row.try_get("id")?),
        user_id: crate::identity::UserId(row.try_get("user_id")?),
        subject: crate::identity::UserContextSubject {
            deployment_id: crate::identity::DeploymentId(row.try_get("deployment_id")?),
            host_app_id: crate::identity::HostAppId(row.try_get("host_app_id")?),
            organization_id: row
                .try_get::<Option<Uuid>, _>("organization_id")?
                .map(crate::identity::HostOrganizationId),
            host_user_id: row.try_get("host_user_id")?,
        },
    })
}
fn webhook_endpoint(value: &str) -> Result<Url, StatusError> {
    let endpoint = Url::parse(value).map_err(|_| StatusError::Invalid)?;
    let host = endpoint.host_str().unwrap_or_default();
    if endpoint.scheme() != "https"
        || endpoint.host_str().is_none()
        || endpoint.username() != ""
        || endpoint.password().is_some()
        || host.eq_ignore_ascii_case("localhost")
        || host.parse::<IpAddr>().ok().is_some_and(private_or_local_ip)
    {
        return Err(StatusError::Invalid);
    }
    Ok(endpoint)
}
fn private_or_local_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
        }
        IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_unspecified()
        }
    }
}
fn new_secret() -> String {
    Uuid::new_v4().to_string()
}
fn secret_hash(secret: &str) -> String {
    hex::encode(Sha256::digest(secret.as_bytes()))
}
fn trimmed(value: &str, max: usize) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max).then_some(value)
}
fn webhook_signature(secret: &str, timestamp: &str, body: &[u8]) -> Result<String, StatusError> {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(secret.as_bytes())
        .map_err(|_| StatusError::Invalid)?;
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn webhook_endpoint_requires_plain_https() {
        assert!(webhook_endpoint("https://host.example/status").is_ok());
        assert!(webhook_endpoint("http://host.example/status").is_err());
        assert!(webhook_endpoint("https://user@host.example/status").is_err());
        assert!(webhook_endpoint("https://127.0.0.1/status").is_err());
        assert!(webhook_endpoint("https://localhost/status").is_err());
    }
    #[test]
    fn webhook_signature_binds_timestamp_and_body() {
        let signature = webhook_signature("secret", "123", b"{} ").unwrap();
        assert_ne!(
            signature,
            webhook_signature("secret", "124", b"{} ").unwrap()
        );
        assert_ne!(
            signature,
            webhook_signature("secret", "123", b"{}").unwrap()
        );
    }
}
