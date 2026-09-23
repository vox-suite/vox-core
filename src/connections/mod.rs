use crate::{
    db::Db,
    identity::{ResolvedUserContext, UserContextId},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialCustody {
    PlatformHeld,
    ExternalOperator,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizationState {
    Pending,
    Authorized,
    Expired,
    Revoked,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthorizeConnectionRequest {
    pub integration_external_key: String,
    pub external_account_reference: String,
    pub credential_custody: CredentialCustody,
    pub authorization_state: AuthorizationState,
    #[serde(default)]
    pub authorized_capabilities: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Connection {
    pub id: Uuid,
    pub user_context_id: UserContextId,
    pub integration_external_key: String,
    pub credential_custody: CredentialCustody,
    pub authorization_state: AuthorizationState,
    pub authorized_capabilities: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Clone)]
pub struct ConnectionService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum ConnectionError {
    #[error("connection request invalid")]
    Invalid,
    #[error("integration unavailable")]
    IntegrationUnavailable,
    #[error("connection unavailable")]
    NotFound,
    #[error("connection storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl ConnectionService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn record(
        &self,
        context: &ResolvedUserContext,
        request: AuthorizeConnectionRequest,
    ) -> Result<Connection, ConnectionError> {
        validate(&request)?;
        let integration=sqlx::query_as::<_,(Uuid,String)>("SELECT id,external_key FROM integration_definitions WHERE deployment_id=$1 AND external_key=$2 AND state='enabled'").bind(context.subject.deployment_id.0).bind(request.integration_external_key.trim()).fetch_optional(self.db.pool()).await?.ok_or(ConnectionError::IntegrationUnavailable)?;
        let id=sqlx::query_scalar::<_,Uuid>("INSERT INTO external_connections (user_context_id,integration_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities,expires_at,failure_code,revoked_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,CASE WHEN $5='revoked' THEN now() ELSE NULL END) ON CONFLICT (user_context_id,integration_id,external_account_hash) DO UPDATE SET credential_custody=EXCLUDED.credential_custody,authorization_state=EXCLUDED.authorization_state,authorized_capabilities=EXCLUDED.authorized_capabilities,expires_at=EXCLUDED.expires_at,failure_code=EXCLUDED.failure_code,revoked_at=EXCLUDED.revoked_at,updated_at=now() RETURNING id").bind(context.id.0).bind(integration.0).bind(hash(&request.external_account_reference)).bind(custody(&request.credential_custody)).bind(state(&request.authorization_state)).bind(&request.authorized_capabilities).bind(request.expires_at).bind(&request.failure_code).fetch_one(self.db.pool()).await?;
        Ok(Connection {
            id,
            user_context_id: context.id,
            integration_external_key: integration.1,
            credential_custody: request.credential_custody,
            authorization_state: request.authorization_state,
            authorized_capabilities: request.authorized_capabilities,
            expires_at: request.expires_at,
            failure_code: request.failure_code,
        })
    }

    /// Returns only connections owned by this authenticated host user context.
    /// Account references are deliberately absent: the current schema stores
    /// only a one-way hash, not a provider-verified display identity.
    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Connection>, ConnectionError> {
        let rows = sqlx::query(
            "SELECT c.id, i.external_key, c.credential_custody, c.authorization_state, \
             c.authorized_capabilities, c.expires_at, c.failure_code \
             FROM external_connections c JOIN integration_definitions i ON i.id=c.integration_id \
             WHERE c.user_context_id=$1 ORDER BY c.created_at DESC, c.id DESC LIMIT 100",
        )
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;
        rows.iter()
            .map(|row| connection_from_row(context.id, row))
            .collect()
    }

    /// Revocation is context-scoped and idempotent. It blocks new Core attempts;
    /// it cannot claim to revoke an external operator's independent access.
    pub async fn disconnect(
        &self,
        context: &ResolvedUserContext,
        connection_id: Uuid,
    ) -> Result<Connection, ConnectionError> {
        let mut tx = self.db.pool().begin().await?;
        let row = sqlx::query(
            "UPDATE external_connections SET authorization_state='revoked', \
             authorized_capabilities='{}'::text[], expires_at=NULL, failure_code=NULL, \
             revoked_at=COALESCE(revoked_at,now()), updated_at=now() \
             WHERE id=$1 AND user_context_id=$2 \
             RETURNING id, integration_id, credential_custody, authorization_state, \
             authorized_capabilities, expires_at, failure_code",
        )
        .bind(connection_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ConnectionError::NotFound)?;
        // A later reconnection must not revive the old agent grants.
        sqlx::query(
            "UPDATE agent_capability_grants SET state='revoked', \
             revoked_at=COALESCE(revoked_at,now()), updated_at=now() \
             WHERE connection_id=$1 AND user_context_id=$2 AND state='enabled'",
        )
        .bind(connection_id)
        .bind(context.id.0)
        .execute(&mut *tx)
        .await?;
        let integration_id: Uuid = row.get("integration_id");
        let integration_key: String =
            sqlx::query_scalar("SELECT external_key FROM integration_definitions WHERE id=$1")
                .bind(integration_id)
                .fetch_one(&mut *tx)
                .await?;
        tx.commit().await?;
        Ok(Connection {
            id: row.get("id"),
            user_context_id: context.id,
            integration_external_key: integration_key,
            credential_custody: parse_custody(row.get("credential_custody"))?,
            authorization_state: AuthorizationState::Revoked,
            authorized_capabilities: row.get("authorized_capabilities"),
            expires_at: row.get("expires_at"),
            failure_code: row.get("failure_code"),
        })
    }
}

fn connection_from_row(
    context_id: UserContextId,
    row: &sqlx::postgres::PgRow,
) -> Result<Connection, ConnectionError> {
    Ok(Connection {
        id: row.get("id"),
        user_context_id: context_id,
        integration_external_key: row.get("external_key"),
        credential_custody: parse_custody(row.get("credential_custody"))?,
        authorization_state: parse_state(row.get("authorization_state"))?,
        authorized_capabilities: row.get("authorized_capabilities"),
        expires_at: row.get("expires_at"),
        failure_code: row.get("failure_code"),
    })
}

fn parse_custody(value: &str) -> Result<CredentialCustody, ConnectionError> {
    match value {
        "platform_held" => Ok(CredentialCustody::PlatformHeld),
        "external_operator" => Ok(CredentialCustody::ExternalOperator),
        _ => Err(ConnectionError::Invalid),
    }
}

fn parse_state(value: &str) -> Result<AuthorizationState, ConnectionError> {
    match value {
        "pending" => Ok(AuthorizationState::Pending),
        "authorized" => Ok(AuthorizationState::Authorized),
        "expired" => Ok(AuthorizationState::Expired),
        "revoked" => Ok(AuthorizationState::Revoked),
        "cancelled" => Ok(AuthorizationState::Cancelled),
        "failed" => Ok(AuthorizationState::Failed),
        _ => Err(ConnectionError::Invalid),
    }
}

fn validate(r: &AuthorizeConnectionRequest) -> Result<(), ConnectionError> {
    if r.integration_external_key.trim().is_empty()
        || r.external_account_reference.trim().is_empty()
        || r.external_account_reference.len() > 512
        || r.authorized_capabilities.len() > 64
    {
        return Err(ConnectionError::Invalid);
    }
    if matches!(r.authorization_state, AuthorizationState::Failed) != r.failure_code.is_some() {
        return Err(ConnectionError::Invalid);
    }
    if !matches!(r.authorization_state, AuthorizationState::Authorized) && r.expires_at.is_some() {
        return Err(ConnectionError::Invalid);
    }
    Ok(())
}

fn hash(v: &str) -> Vec<u8> {
    Sha256::digest(v.trim().as_bytes()).to_vec()
}

fn custody(v: &CredentialCustody) -> &'static str {
    match v {
        CredentialCustody::PlatformHeld => "platform_held",
        CredentialCustody::ExternalOperator => "external_operator",
    }
}

fn state(v: &AuthorizationState) -> &'static str {
    match v {
        AuthorizationState::Pending => "pending",
        AuthorizationState::Authorized => "authorized",
        AuthorizationState::Expired => "expired",
        AuthorizationState::Revoked => "revoked",
        AuthorizationState::Cancelled => "cancelled",
        AuthorizationState::Failed => "failed",
    }
}
