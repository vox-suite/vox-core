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
    #[serde(default)]
    pub account_display_id: Option<String>,
    pub credential_custody: CredentialCustody,
    pub authorization_state: AuthorizationState,
    #[serde(default)]
    pub authorized_capabilities: Vec<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub failure_code: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InitiateConnectionRequest {
    pub integration_external_key: String,
    pub credential_custody: CredentialCustody,
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
    pub redirect_uri: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct InitiateConnectionResponse {
    pub session_id: Uuid,
    pub integration_external_key: String,
    pub state_token: String,
    pub authorization_url: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VerifyConnectionCallbackRequest {
    pub session_id: Uuid,
    pub state_token: String,
    pub provider_code: String,
    pub external_account_reference: String,
    pub account_display_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Connection {
    pub id: Uuid,
    pub user_context_id: UserContextId,
    pub integration_external_key: String,
    pub account_display_id: Option<String>,
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
    #[error("session already consumed")]
    SessionAlreadyConsumed,
    #[error("session expired")]
    SessionExpired,
    #[error("invalid state challenge")]
    InvalidState,
    #[error("connection storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl ConnectionService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn initiate(
        &self,
        context: &ResolvedUserContext,
        request: InitiateConnectionRequest,
    ) -> Result<InitiateConnectionResponse, ConnectionError> {
        let integration = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, external_key FROM integration_definitions \
             WHERE deployment_id=$1 AND external_key=$2 AND state='enabled'",
        )
        .bind(context.subject.deployment_id.0)
        .bind(request.integration_external_key.trim())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ConnectionError::IntegrationUnavailable)?;

        let session_id = Uuid::new_v4();
        let state_token = format!("{}-{}", Uuid::new_v4(), Uuid::new_v4());
        let expires_at = Utc::now() + chrono::Duration::minutes(10);

        sqlx::query(
            "INSERT INTO connection_authorization_sessions \
             (id, user_context_id, integration_id, state_token, credential_custody, requested_capabilities, redirect_uri, expires_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(session_id)
        .bind(context.id.0)
        .bind(integration.0)
        .bind(&state_token)
        .bind(custody(&request.credential_custody))
        .bind(&request.requested_capabilities)
        .bind(&request.redirect_uri)
        .bind(expires_at)
        .execute(self.db.pool())
        .await?;

        let authorization_url = format!(
            "/v1/integrations/{}/oauth/authorize?state={}&session={}",
            integration.1, state_token, session_id
        );

        Ok(InitiateConnectionResponse {
            session_id,
            integration_external_key: integration.1,
            state_token,
            authorization_url,
            expires_at,
        })
    }

    pub async fn verify_callback(
        &self,
        context: &ResolvedUserContext,
        request: VerifyConnectionCallbackRequest,
    ) -> Result<Connection, ConnectionError> {
        if request.state_token.trim().is_empty()
            || request.provider_code.trim().is_empty()
            || request.external_account_reference.trim().is_empty()
            || request.account_display_id.trim().is_empty()
        {
            return Err(ConnectionError::Invalid);
        }

        let mut tx = self.db.pool().begin().await?;

        let row = sqlx::query(
            "SELECT id, integration_id, state_token, credential_custody, requested_capabilities, expires_at, consumed_at \
             FROM connection_authorization_sessions \
             WHERE id=$1 AND user_context_id=$2 FOR UPDATE",
        )
        .bind(request.session_id)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(ConnectionError::NotFound)?;

        let consumed_at: Option<DateTime<Utc>> = row.get("consumed_at");
        if consumed_at.is_some() {
            return Err(ConnectionError::SessionAlreadyConsumed);
        }

        let expires_at: DateTime<Utc> = row.get("expires_at");
        if expires_at < Utc::now() {
            return Err(ConnectionError::SessionExpired);
        }

        let expected_state: String = row.get("state_token");
        if expected_state != request.state_token {
            return Err(ConnectionError::InvalidState);
        }

        sqlx::query(
            "UPDATE connection_authorization_sessions SET consumed_at=now() WHERE id=$1",
        )
        .bind(request.session_id)
        .execute(&mut *tx)
        .await?;

        let integration_id: Uuid = row.get("integration_id");
        let custody_str: String = row.get("credential_custody");
        let requested_capabilities: Vec<String> = row.get("requested_capabilities");
        let credential_custody = parse_custody(&custody_str)?;

        let integration_key: String = sqlx::query_scalar(
            "SELECT external_key FROM integration_definitions WHERE id=$1",
        )
        .bind(integration_id)
        .fetch_one(&mut *tx)
        .await?;

        let expires_at_conn = Some(Utc::now() + chrono::Duration::days(30));
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO external_connections \
             (user_context_id, integration_id, external_account_hash, account_display_id, credential_custody, authorization_state, authorized_capabilities, expires_at, failure_code, revoked_at) \
             VALUES ($1, $2, $3, $4, $5, 'authorized', $6, $7, NULL, NULL) \
             ON CONFLICT (user_context_id, integration_id, external_account_hash) \
             DO UPDATE SET account_display_id=EXCLUDED.account_display_id, \
                           credential_custody=EXCLUDED.credential_custody, \
                           authorization_state='authorized', \
                           authorized_capabilities=EXCLUDED.authorized_capabilities, \
                           expires_at=EXCLUDED.expires_at, \
                           failure_code=NULL, \
                           revoked_at=NULL, \
                           updated_at=now() \
             RETURNING id",
        )
        .bind(context.id.0)
        .bind(integration_id)
        .bind(hash(&request.external_account_reference))
        .bind(request.account_display_id.trim())
        .bind(custody(&credential_custody))
        .bind(&requested_capabilities)
        .bind(expires_at_conn)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;

        Ok(Connection {
            id,
            user_context_id: context.id,
            integration_external_key: integration_key,
            account_display_id: Some(request.account_display_id.trim().to_string()),
            credential_custody,
            authorization_state: AuthorizationState::Authorized,
            authorized_capabilities: requested_capabilities,
            expires_at: expires_at_conn,
            failure_code: None,
        })
    }

    pub async fn record(
        &self,
        context: &ResolvedUserContext,
        request: AuthorizeConnectionRequest,
    ) -> Result<Connection, ConnectionError> {
        validate(&request)?;
        let integration = sqlx::query_as::<_, (Uuid, String)>(
            "SELECT id, external_key FROM integration_definitions \
             WHERE deployment_id=$1 AND external_key=$2 AND state='enabled'",
        )
        .bind(context.subject.deployment_id.0)
        .bind(request.integration_external_key.trim())
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(ConnectionError::IntegrationUnavailable)?;

        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO external_connections \
             (user_context_id, integration_id, external_account_hash, account_display_id, credential_custody, authorization_state, authorized_capabilities, expires_at, failure_code, revoked_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, CASE WHEN $6='revoked' THEN now() ELSE NULL END) \
             ON CONFLICT (user_context_id, integration_id, external_account_hash) \
             DO UPDATE SET account_display_id=COALESCE(EXCLUDED.account_display_id, external_connections.account_display_id), \
                           credential_custody=EXCLUDED.credential_custody, \
                           authorization_state=EXCLUDED.authorization_state, \
                           authorized_capabilities=EXCLUDED.authorized_capabilities, \
                           expires_at=EXCLUDED.expires_at, \
                           failure_code=EXCLUDED.failure_code, \
                           revoked_at=EXCLUDED.revoked_at, \
                           updated_at=now() \
             RETURNING id",
        )
        .bind(context.id.0)
        .bind(integration.0)
        .bind(hash(&request.external_account_reference))
        .bind(&request.account_display_id)
        .bind(custody(&request.credential_custody))
        .bind(state(&request.authorization_state))
        .bind(&request.authorized_capabilities)
        .bind(request.expires_at)
        .bind(&request.failure_code)
        .fetch_one(self.db.pool())
        .await?;

        Ok(Connection {
            id,
            user_context_id: context.id,
            integration_external_key: integration.1,
            account_display_id: request.account_display_id,
            credential_custody: request.credential_custody,
            authorization_state: request.authorization_state,
            authorized_capabilities: request.authorized_capabilities,
            expires_at: request.expires_at,
            failure_code: request.failure_code,
        })
    }

    /// Returns only connections owned by this authenticated host user context.
    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<Connection>, ConnectionError> {
        let rows = sqlx::query(
            "SELECT c.id, i.external_key, c.account_display_id, c.credential_custody, c.authorization_state, \
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
             RETURNING id, integration_id, account_display_id, credential_custody, authorization_state, \
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
            account_display_id: row.try_get("account_display_id").ok(),
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
        account_display_id: row.try_get("account_display_id").ok(),
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
