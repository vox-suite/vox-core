use crate::{
    db::Db,
    identity::{ResolvedUserContext, UserContextId},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
        let provider_key = request.integration_external_key.trim();
        if provider_key.is_empty() {
            return Err(ConnectionError::Invalid);
        }
        let account_hash = hash(&request.external_account_reference);
        let secret_reference = format!("platform:{account_hash}");
        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO connections (user_id, provider_key, external_account_hash, secret_reference, allowed_capabilities, authorization_state, expires_at, revoked_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, CASE WHEN $6 = 'revoked' THEN now() ELSE NULL END) \
             ON CONFLICT (user_id, provider_key, external_account_hash) DO UPDATE \
             SET secret_reference = EXCLUDED.secret_reference, \
                 allowed_capabilities = EXCLUDED.allowed_capabilities, \
                 authorization_state = EXCLUDED.authorization_state, \
                 expires_at = EXCLUDED.expires_at, \
                 revoked_at = EXCLUDED.revoked_at, \
                 updated_at = now() \
             RETURNING id",
        )
        .bind(context.user_id.0)
        .bind(provider_key)
        .bind(&account_hash)
        .bind(&secret_reference)
        .bind(&request.authorized_capabilities)
        .bind(state(&request.authorization_state))
        .bind(request.expires_at)
        .fetch_one(self.db.pool())
        .await?;
        Ok(Connection {
            id,
            user_context_id: context.id,
            integration_external_key: provider_key.to_owned(),
            credential_custody: request.credential_custody,
            authorization_state: request.authorization_state,
            authorized_capabilities: request.authorized_capabilities,
            expires_at: request.expires_at,
            failure_code: request.failure_code,
        })
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
    if !matches!(r.authorization_state, AuthorizationState::Authorized) && r.expires_at.is_some()
    {
        return Err(ConnectionError::Invalid);
    }
    Ok(())
}

fn hash(v: &str) -> String {
    hex::encode(Sha256::digest(v.trim().as_bytes()))
}

fn state(v: &AuthorizationState) -> &'static str {
    match v {
        AuthorizationState::Pending => "pending",
        AuthorizationState::Authorized => "authorized",
        AuthorizationState::Expired => "expired",
        AuthorizationState::Revoked => "revoked",
        AuthorizationState::Cancelled | AuthorizationState::Failed => "revoked",
    }
}
