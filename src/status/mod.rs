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
use url::{Host, Url};
use uuid::Uuid;

mod secrets;
pub use secrets::EncryptedWebhookSecretStore;

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
    pub secret_version: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
}

#[async_trait::async_trait]
pub trait WebhookSecretStore: Send + Sync {
    async fn put_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        subscription_id: Uuid,
        secret: String,
    ) -> Result<(), StatusError>;
    async fn get(&self, subscription_id: Uuid) -> Result<String, StatusError>;
    async fn get_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        subscription_id: Uuid,
    ) -> Result<String, StatusError>;
    async fn delete_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        subscription_id: Uuid,
    ) -> Result<(), StatusError>;
}

#[derive(Clone)]
pub struct UnavailableWebhookSecretStore;

#[async_trait::async_trait]
impl WebhookSecretStore for UnavailableWebhookSecretStore {
    async fn put_in_transaction(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _: Uuid,
        _: String,
    ) -> Result<(), StatusError> {
        Err(StatusError::Unavailable)
    }
    async fn get(&self, _: Uuid) -> Result<String, StatusError> {
        Err(StatusError::Unavailable)
    }
    async fn get_in_transaction(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _: Uuid,
    ) -> Result<String, StatusError> {
        Err(StatusError::Unavailable)
    }
    async fn delete_in_transaction(
        &self,
        _: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        _: Uuid,
    ) -> Result<(), StatusError> {
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
        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "INSERT INTO status_webhook_subscriptions (id,user_context_id,endpoint,state)
             VALUES ($1,$2,$3,'disabled')",
        )
        .bind(id)
        .bind(context.id.0)
        .bind(&endpoint)
        .execute(&mut *tx)
        .await?;
        self.secrets
            .put_in_transaction(&mut tx, id, secret.clone())
            .await?;
        sqlx::query(
            "UPDATE status_webhook_subscriptions SET state='enabled',updated_at=now() WHERE id=$1",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(WebhookSubscription {
            id,
            endpoint,
            state: "enabled".into(),
            secret_version: 1,
            secret: Some(secret),
        })
    }

    pub async fn rotate_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<WebhookSubscription, StatusError> {
        let mut tx = self.db.pool().begin().await?;
        let endpoint = sqlx::query_scalar::<_, String>(
            "SELECT endpoint FROM status_webhook_subscriptions
             WHERE id=$1 AND user_context_id=$2 AND state='enabled' FOR UPDATE",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?;
        let endpoint = endpoint.ok_or(StatusError::NotFound)?;
        // A missing or wrong custody key must not overwrite the only secret.
        self.secrets.get_in_transaction(&mut tx, id).await?;
        let secret = new_webhook_secret();
        self.secrets
            .put_in_transaction(&mut tx, id, secret.clone())
            .await?;
        let version: i32 = sqlx::query_scalar(
            "UPDATE status_webhook_subscriptions SET secret_version=secret_version+1, updated_at=now()
             WHERE id=$1 RETURNING secret_version",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(WebhookSubscription {
            id,
            endpoint,
            state: "enabled".into(),
            secret_version: version,
            secret: Some(secret),
        })
    }

    pub async fn disable_subscription(
        &self,
        context: &ResolvedUserContext,
        id: Uuid,
    ) -> Result<(), StatusError> {
        let mut tx = self.db.pool().begin().await?;
        let changed = sqlx::query(
            "UPDATE status_webhook_subscriptions SET state='disabled', updated_at=now()
             WHERE id=$1 AND user_context_id=$2 AND state IN ('enabled','unhealthy')",
        )
        .bind(id)
        .bind(context.id.0)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if changed == 0 {
            return Err(StatusError::NotFound);
        }
        self.secrets.delete_in_transaction(&mut tx, id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn list_subscriptions(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<WebhookSubscription>, StatusError> {
        let rows = sqlx::query(
            "SELECT id, endpoint, state, secret_version FROM status_webhook_subscriptions
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
                secret_version: row.get("secret_version"),
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
        let event_id = trimmed(&incoming.provider_event_id, 255).ok_or(StatusError::Invalid)?;
        let account_hash = hex::encode(Sha256::digest(
            trimmed(&incoming.external_account_reference, 512).ok_or(StatusError::Invalid)?,
        ));
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "SELECT e.user_id, e.provider_reference, uc.id AS context_id, \
                    uc.deployment_id, uc.host_app_id, uc.organization_id, uc.host_user_id \
             FROM executions e \
             JOIN connections c ON c.id = e.connection_id \
               AND c.user_id = e.user_id AND c.user_context_id = e.user_context_id \
             JOIN user_contexts uc ON uc.id = e.user_context_id AND uc.user_id = e.user_id \
             JOIN integration_definitions i ON i.deployment_id = uc.deployment_id \
               AND i.external_key = c.provider_key AND i.state = 'enabled' \
             WHERE e.id = $1 AND c.provider_key = $2 AND c.external_account_hash = $3
             FOR UPDATE OF e FOR SHARE OF c, i",
        )
        .bind(incoming.execution_id)
        .bind(provider_key)
        .bind(&account_hash)
        .fetch_optional(&mut *tx)
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
        let inserted = sqlx::query(
            "INSERT INTO verified_integration_events
             (integration_external_key,external_account_hash,provider_event_id,execution_id)
             VALUES ($1,$2,$3,$4) ON CONFLICT DO NOTHING",
        )
        .bind(provider_key)
        .bind(&account_hash)
        .bind(event_id)
        .bind(incoming.execution_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if inserted == 0 {
            let recorded: Uuid = sqlx::query_scalar(
                "SELECT execution_id FROM verified_integration_events
                 WHERE integration_external_key=$1 AND external_account_hash=$2 AND provider_event_id=$3",
            )
            .bind(provider_key)
            .bind(&account_hash)
            .bind(event_id)
            .fetch_one(&mut *tx)
            .await?;
            if recorded != incoming.execution_id {
                return Err(StatusError::Invalid);
            }
            tx.commit().await?;
            return coordinator
                .get(&context, incoming.execution_id)
                .await
                .map_err(Into::into);
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

pub struct WebhookDeliveryWorker {
    db: Db,
    secrets: Arc<dyn WebhookSecretStore>,
    transport: Arc<dyn WebhookTransport>,
}

pub struct WebhookRequest {
    pub endpoint: String,
    pub delivery_id: Uuid,
    pub timestamp: String,
    pub signature: String,
    pub body: Vec<u8>,
}

#[async_trait::async_trait]
pub trait WebhookTransport: Send + Sync {
    async fn post(&self, request: WebhookRequest) -> Option<u16>;
}

pub struct PublicHttpsWebhookTransport;

#[async_trait::async_trait]
impl WebhookTransport for PublicHttpsWebhookTransport {
    async fn post(&self, request: WebhookRequest) -> Option<u16> {
        webhook_endpoint(&request.endpoint).ok()?;
        let url = Url::parse(&request.endpoint).ok()?;
        let host = url.host()?;
        let port = url.port_or_known_default()?;
        let mut client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10));
        match host {
            Host::Domain(domain) => {
                let address = tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::net::lookup_host((domain, port)),
                )
                .await
                .ok()?
                .ok()?
                .find(|address| public_ip(address.ip()))?;
                client = client.resolve(domain, address);
            }
            Host::Ipv4(ip) if public_ip(IpAddr::V4(ip)) => {}
            Host::Ipv6(ip) if public_ip(IpAddr::V6(ip)) => {}
            _ => return None,
        }
        let client = client.build().ok()?;
        client
            .post(request.endpoint)
            .header("X-Vox-Signature-Version", "v1")
            .header("X-Vox-Delivery-Id", request.delivery_id.to_string())
            .header("X-Vox-Timestamp", request.timestamp)
            .header("X-Vox-Signature", request.signature)
            .header("Content-Type", "application/json")
            .body(request.body)
            .send()
            .await
            .ok()
            .map(|response| response.status().as_u16())
    }
}

impl WebhookDeliveryWorker {
    pub fn new(db: Db, secrets: Arc<dyn WebhookSecretStore>) -> Self {
        Self {
            db,
            secrets,
            transport: Arc::new(PublicHttpsWebhookTransport),
        }
    }

    pub fn with_transport(mut self, transport: Arc<dyn WebhookTransport>) -> Self {
        self.transport = transport;
        self
    }

    pub async fn deliver_next(
        &self,
        worker: &str,
        now: DateTime<Utc>,
    ) -> Result<bool, StatusError> {
        if trimmed(worker, 128).is_none() {
            return Err(StatusError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "SELECT d.id, d.subscription_id, d.attempts, s.endpoint,
                    e.cursor, e.aggregate_type, e.aggregate_id, e.event_type,
                    e.state AS event_state, e.occurred_at
             FROM status_webhook_deliveries d
             JOIN status_webhook_subscriptions s ON s.id=d.subscription_id
             JOIN status_events e ON e.cursor=d.event_cursor
             WHERE s.state='enabled' AND
               ((d.state='queued' AND d.available_at <= $1) OR
                (d.state='sending' AND d.lease_until <= $1))
             ORDER BY d.available_at, d.id
             LIMIT 1 FOR UPDATE OF d SKIP LOCKED",
        )
        .bind(now)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(row) = row else {
            tx.commit().await?;
            return Ok(false);
        };
        let id: Uuid = row.get("id");
        let lease_token = Uuid::new_v4();
        let subscription_id: Uuid = row.get("subscription_id");
        let attempts: i32 = row.get("attempts");
        let endpoint: String = row.get("endpoint");
        let body = serde_json::to_vec(&serde_json::json!({
            "version": "v1",
            "delivery_id": id,
            "cursor": row.get::<i64, _>("cursor"),
            "aggregate_type": row.get::<String, _>("aggregate_type"),
            "aggregate_id": row.get::<Uuid, _>("aggregate_id"),
            "event_type": row.get::<String, _>("event_type"),
            "state": row.get::<String, _>("event_state"),
            "occurred_at": row.get::<DateTime<Utc>, _>("occurred_at"),
            "authoritative": false,
        }))
        .map_err(|_| StatusError::Invalid)?;
        sqlx::query(
            "UPDATE status_webhook_deliveries
             SET state='sending',attempts=attempts+1,lease_until=$2,
                 lease_token=$3,lease_owner=$4
             WHERE id=$1",
        )
        .bind(id)
        .bind(now + chrono::Duration::seconds(30))
        .bind(lease_token)
        .bind(worker)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        // An expired lease may be retried with the same delivery ID. Hosts
        // deduplicate that ID and fetch authoritative state after the hint.
        let http_status = self.send(&endpoint, subscription_id, id, now, body).await;
        let finished_at = Utc::now();
        if http_status.is_some_and(|status| (200..300).contains(&status)) {
            sqlx::query(
                "UPDATE status_webhook_deliveries
                 SET state='sent',lease_until=NULL,lease_token=NULL,lease_owner=NULL,
                     delivered_at=$2,last_http_status=$3
                 WHERE id=$1 AND state='sending' AND lease_token=$4",
            )
            .bind(id)
            .bind(finished_at)
            .bind(http_status.map(i32::from))
            .bind(lease_token)
            .execute(self.db.pool())
            .await?;
        } else if attempts + 1 >= 8 {
            let mut tx = self.db.pool().begin().await?;
            let changed = sqlx::query(
                "UPDATE status_webhook_deliveries
                 SET state='failed',lease_until=NULL,lease_token=NULL,lease_owner=NULL,
                     last_http_status=$2
                 WHERE id=$1 AND state='sending' AND lease_token=$3",
            )
            .bind(id)
            .bind(http_status.map(i32::from))
            .bind(lease_token)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if changed > 0 {
                sqlx::query(
                    "UPDATE status_webhook_subscriptions SET state='unhealthy',updated_at=$2
                     WHERE id=$1 AND state='enabled'",
                )
                .bind(subscription_id)
                .bind(finished_at)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
        } else {
            let delay = 1_i64 << (attempts + 1).min(8);
            sqlx::query(
                "UPDATE status_webhook_deliveries
                 SET state='queued',lease_until=NULL,lease_token=NULL,lease_owner=NULL,
                     available_at=$2,last_http_status=$3
                 WHERE id=$1 AND state='sending' AND lease_token=$4",
            )
            .bind(id)
            .bind(finished_at + chrono::Duration::seconds(delay))
            .bind(http_status.map(i32::from))
            .bind(lease_token)
            .execute(self.db.pool())
            .await?;
        }
        Ok(true)
    }

    async fn send(
        &self,
        endpoint: &str,
        subscription_id: Uuid,
        delivery_id: Uuid,
        now: DateTime<Utc>,
        body: Vec<u8>,
    ) -> Option<u16> {
        let secret = self.secrets.get(subscription_id).await.ok()?;
        let timestamp = now.timestamp().to_string();
        let signature = webhook_signature(&secret, &timestamp, &body).ok()?;
        self.transport
            .post(WebhookRequest {
                endpoint: endpoint.to_owned(),
                delivery_id,
                timestamp,
                signature,
                body,
            })
            .await
    }
}

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let first = ip.octets()[0];
            !(first == 0
                || first == 10
                || first == 127
                || first >= 224
                || ip.is_private()
                || ip.is_link_local()
                || ip.is_broadcast()
                || ip.is_documentation()
                || (first == 100 && (64..=127).contains(&ip.octets()[1]))
                || (first == 198 && (18..=19).contains(&ip.octets()[1])))
        }
        IpAddr::V6(ip) => {
            if let Some(mapped) = ip.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            !(ip.is_unspecified()
                || ip.is_loopback()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast())
        }
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
        || parsed.host().is_none_or(|host| match host {
            Host::Domain(domain) => domain.eq_ignore_ascii_case("localhost"),
            Host::Ipv4(ip) => !public_ip(IpAddr::V4(ip)),
            Host::Ipv6(ip) => !public_ip(IpAddr::V6(ip)),
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
