use crate::{
    db::Db,
    identity::{
        DeploymentId, HostAppId, HostOrganizationId, IdentityError, IdentityService,
        ResolvedUserContext, UserContextSubject,
    },
};
use chrono::{DateTime, Duration, Utc};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use subtle::ConstantTimeEq;
use url::Url;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const HOST_CONTEXT_PATH: &str = "/v1/host/context";
pub const HOST_ASSERTION_MAX_AGE_SECONDS: i64 = 300;
const MAX_HOST_KEY_BYTES: usize = 255;
const MAX_ORIGIN_COUNT: usize = 32;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterHostAppRequest {
    pub deployment_external_key: String,
    pub host_app_external_key: String,
    #[serde(default)]
    pub allowed_origins: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostContextRequest {
    pub host_user_id: String,
    #[serde(default)]
    pub organization_external_key: Option<String>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct HostAppCredential {
    pub credential_id: Uuid,
    pub audience: String,
    pub secret: String,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisteredHostApp {
    pub deployment_id: DeploymentId,
    pub host_app_id: HostAppId,
    pub deployment_external_key: String,
    pub host_app_external_key: String,
    pub allowed_origins: Vec<String>,
    pub credential: HostAppCredential,
}

#[derive(Clone, Eq, PartialEq)]
pub struct HostContextAssertion {
    credential_id: Uuid,
    audience: String,
    issued_at: DateTime<Utc>,
    nonce: Uuid,
    signature: String,
    secret: String,
}

impl HostContextAssertion {
    pub fn credential_id(&self) -> Uuid {
        self.credential_id
    }

    pub fn audience(&self) -> &str {
        &self.audience
    }

    pub fn issued_at(&self) -> DateTime<Utc> {
        self.issued_at
    }

    pub fn nonce(&self) -> Uuid {
        self.nonce
    }

    pub fn signature(&self) -> &str {
        &self.signature
    }

    pub fn secret(&self) -> &str {
        &self.secret
    }

    pub fn from_parts(
        credential_id: Uuid,
        audience: String,
        issued_at: DateTime<Utc>,
        nonce: Uuid,
        signature: String,
        secret: String,
    ) -> Self {
        Self {
            credential_id,
            audience,
            issued_at,
            nonce,
            signature,
            secret,
        }
    }
}

impl HostAppCredential {
    pub fn sign_context_request(
        &self,
        request: &HostContextRequest,
        issued_at: DateTime<Utc>,
        nonce: Uuid,
    ) -> Result<HostContextAssertion, HostTrustError> {
        let request = NormalizedHostContextRequest::from_request(request)?;
        let signature = assertion_signature(
            &self.secret,
            self.credential_id,
            &self.audience,
            issued_at,
            nonce,
            &request,
        )?;
        Ok(HostContextAssertion {
            credential_id: self.credential_id,
            audience: self.audience.clone(),
            issued_at,
            nonce,
            signature,
            secret: self.secret.clone(),
        })
    }
}

#[derive(Clone)]
pub struct HostTrustService {
    db: Db,
    identities: IdentityService,
}

#[derive(Debug, thiserror::Error)]
pub enum HostTrustError {
    #[error("host registration is invalid")]
    InvalidRegistration,
    #[error("host context request is invalid")]
    InvalidRequest,
    #[error("host assertion is invalid")]
    InvalidAssertion,
    #[error("host assertion is denied")]
    AuthenticationDenied,
    #[error("host assertion has expired")]
    AssertionExpired,
    #[error("host assertion has already been used")]
    AssertionReplayed,
    #[error("host origin is not allowed")]
    OriginDenied,
    #[error("host credential is not registered")]
    CredentialNotFound,
    #[error("host trust storage is unavailable")]
    Database(#[from] sqlx::Error),
    #[error("host context resolution failed")]
    Identity(#[from] IdentityError),
}

impl HostTrustService {
    pub fn new(db: Db) -> Self {
        Self {
            identities: IdentityService::new(db.clone()),
            db,
        }
    }

    pub async fn register_host_app(
        &self,
        request: RegisterHostAppRequest,
    ) -> Result<RegisteredHostApp, HostTrustError> {
        let deployment_external_key = normalize_key(&request.deployment_external_key)
            .ok_or(HostTrustError::InvalidRegistration)?;
        let host_app_external_key = normalize_key(&request.host_app_external_key)
            .ok_or(HostTrustError::InvalidRegistration)?;
        let allowed_origins = normalize_origins(request.allowed_origins)?;

        let mut tx = self.db.pool().begin().await?;
        let deployment_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO platform_deployments (external_key) VALUES ($1) \
             ON CONFLICT (external_key) DO UPDATE SET external_key = EXCLUDED.external_key \
             RETURNING id",
        )
        .bind(&deployment_external_key)
        .fetch_one(&mut *tx)
        .await?;
        let host_app_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO host_apps (deployment_id, external_key, allowed_origins) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (deployment_id, external_key) \
             DO UPDATE SET allowed_origins = EXCLUDED.allowed_origins \
             RETURNING id",
        )
        .bind(deployment_id)
        .bind(&host_app_external_key)
        .bind(&allowed_origins)
        .fetch_one(&mut *tx)
        .await?;
        let credential = insert_credential(
            &mut tx,
            deployment_id,
            host_app_id,
            audience(&deployment_external_key, &host_app_external_key),
        )
        .await?;
        tx.commit().await?;

        Ok(RegisteredHostApp {
            deployment_id: DeploymentId(deployment_id),
            host_app_id: HostAppId(host_app_id),
            deployment_external_key,
            host_app_external_key,
            allowed_origins,
            credential,
        })
    }

    pub async fn rotate_credential(
        &self,
        host_app_id: HostAppId,
    ) -> Result<HostAppCredential, HostTrustError> {
        let mut tx = self.db.pool().begin().await?;
        let host = sqlx::query_as::<_, (Uuid, String, String)>(
            "SELECT h.deployment_id, d.external_key, h.external_key \
             FROM host_apps h JOIN platform_deployments d ON d.id = h.deployment_id \
             WHERE h.id = $1",
        )
        .bind(host_app_id.0)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(HostTrustError::CredentialNotFound)?;
        let credential =
            insert_credential(&mut tx, host.0, host_app_id.0, audience(&host.1, &host.2)).await?;
        tx.commit().await?;
        Ok(credential)
    }

    pub async fn revoke_credential(&self, credential_id: Uuid) -> Result<(), HostTrustError> {
        let result = sqlx::query(
            "UPDATE host_app_credentials \
             SET state = 'revoked', revoked_at = now() \
             WHERE id = $1 AND state = 'active'",
        )
        .bind(credential_id)
        .execute(self.db.pool())
        .await?;
        if result.rows_affected() == 1 {
            Ok(())
        } else {
            Err(HostTrustError::CredentialNotFound)
        }
    }

    pub async fn resolve_authenticated_context(
        &self,
        assertion: &HostContextAssertion,
        request: &HostContextRequest,
        origin: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<ResolvedUserContext, HostTrustError> {
        let request = NormalizedHostContextRequest::from_request(request)?;
        let credential = sqlx::query_as::<_, (Uuid, Uuid, Vec<u8>, String, String, Vec<String>)>(
            "SELECT c.deployment_id, c.host_app_id, c.secret_hash, d.external_key, h.external_key, h.allowed_origins \
             FROM host_app_credentials c \
             JOIN platform_deployments d ON d.id = c.deployment_id \
             JOIN host_apps h ON h.id = c.host_app_id AND h.deployment_id = c.deployment_id \
             WHERE c.id = $1 AND c.state = 'active'",
        )
        .bind(assertion.credential_id)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(HostTrustError::AuthenticationDenied)?;

        let expected_audience = audience(&credential.3, &credential.4);
        if !bool::from(
            assertion
                .audience
                .as_bytes()
                .ct_eq(expected_audience.as_bytes()),
        ) {
            return Err(HostTrustError::AuthenticationDenied);
        }
        if assertion.issued_at > now + Duration::seconds(60)
            || assertion.issued_at + Duration::seconds(HOST_ASSERTION_MAX_AGE_SECONDS) < now
        {
            return Err(HostTrustError::AssertionExpired);
        }
        if let Some(origin) = origin {
            let origin = normalize_origin(origin).ok_or(HostTrustError::OriginDenied)?;
            if !credential.5.iter().any(|allowed| allowed == &origin) {
                return Err(HostTrustError::OriginDenied);
            }
        }
        let provided_hash = Sha256::digest(assertion.secret.as_bytes());
        if !bool::from(provided_hash.as_slice().ct_eq(credential.2.as_slice())) {
            return Err(HostTrustError::AuthenticationDenied);
        }
        let signature =
            hex::decode(&assertion.signature).map_err(|_| HostTrustError::InvalidAssertion)?;
        let mut verifier = HmacSha256::new_from_slice(assertion.secret.as_bytes())
            .map_err(|_| HostTrustError::InvalidAssertion)?;
        verifier.update(
            canonical_assertion(
                assertion.credential_id,
                &assertion.audience,
                assertion.issued_at,
                assertion.nonce,
                &request,
            )
            .as_bytes(),
        );
        verifier
            .verify_slice(&signature)
            .map_err(|_| HostTrustError::AuthenticationDenied)?;

        sqlx::query("DELETE FROM host_app_assertion_nonces WHERE expires_at < $1")
            .bind(now)
            .execute(self.db.pool())
            .await?;
        let inserted_nonce = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO host_app_assertion_nonces (credential_id, nonce, expires_at) \
             VALUES ($1, $2, $3) \
             ON CONFLICT DO NOTHING RETURNING nonce",
        )
        .bind(assertion.credential_id)
        .bind(assertion.nonce)
        .bind(assertion.issued_at + Duration::seconds(HOST_ASSERTION_MAX_AGE_SECONDS))
        .fetch_optional(self.db.pool())
        .await?;
        if inserted_nonce.is_none() {
            return Err(HostTrustError::AssertionReplayed);
        }

        let organization_id = if let Some(external_key) = request.organization_external_key {
            Some(
                sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM host_organizations \
                 WHERE deployment_id = $1 AND host_app_id = $2 AND external_key = $3",
                )
                .bind(credential.0)
                .bind(credential.1)
                .bind(external_key)
                .fetch_optional(self.db.pool())
                .await?
                .map(HostOrganizationId)
                .ok_or(HostTrustError::AuthenticationDenied)?,
            )
        } else {
            None
        };
        self.identities
            .resolve_context(&UserContextSubject {
                deployment_id: DeploymentId(credential.0),
                host_app_id: HostAppId(credential.1),
                organization_id,
                host_user_id: request.host_user_id,
            })
            .await
            .map_err(HostTrustError::from)
    }
}

#[derive(Clone)]
struct NormalizedHostContextRequest {
    host_user_id: String,
    organization_external_key: Option<String>,
}

impl NormalizedHostContextRequest {
    fn from_request(request: &HostContextRequest) -> Result<Self, HostTrustError> {
        let host_user_id = request.host_user_id.trim();
        if host_user_id.is_empty() || host_user_id.len() > 512 {
            return Err(HostTrustError::InvalidRequest);
        }
        let organization_external_key = match request.organization_external_key.as_deref() {
            Some(value) => Some(normalize_key(value).ok_or(HostTrustError::InvalidRequest)?),
            None => None,
        };
        Ok(Self {
            host_user_id: host_user_id.to_owned(),
            organization_external_key,
        })
    }
}

async fn insert_credential(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: Uuid,
    host_app_id: Uuid,
    audience: String,
) -> Result<HostAppCredential, HostTrustError> {
    let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
    let hash = Sha256::digest(secret.as_bytes()).to_vec();
    let credential_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO host_app_credentials (deployment_id, host_app_id, secret_hash) \
         VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(deployment_id)
    .bind(host_app_id)
    .bind(hash)
    .fetch_one(&mut **tx)
    .await?;
    Ok(HostAppCredential {
        credential_id,
        audience,
        secret,
    })
}

fn normalize_key(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_HOST_KEY_BYTES {
        None
    } else {
        Some(value.to_owned())
    }
}

fn normalize_origins(origins: Vec<String>) -> Result<Vec<String>, HostTrustError> {
    if origins.len() > MAX_ORIGIN_COUNT {
        return Err(HostTrustError::InvalidRegistration);
    }
    let mut normalized = Vec::with_capacity(origins.len());
    for origin in origins {
        let origin = normalize_origin(&origin).ok_or(HostTrustError::InvalidRegistration)?;
        if !normalized.contains(&origin) {
            normalized.push(origin);
        }
    }
    Ok(normalized)
}

fn normalize_origin(value: &str) -> Option<String> {
    let parsed = Url::parse(value.trim()).ok()?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || parsed.path() != "/"
    {
        return None;
    }
    let origin = parsed.origin();
    if origin.is_tuple() {
        Some(origin.ascii_serialization())
    } else {
        None
    }
}

fn audience(deployment_external_key: &str, host_app_external_key: &str) -> String {
    format!("vox-host:{deployment_external_key}:{host_app_external_key}")
}

fn assertion_signature(
    secret: &str,
    credential_id: Uuid,
    audience: &str,
    issued_at: DateTime<Utc>,
    nonce: Uuid,
    request: &NormalizedHostContextRequest,
) -> Result<String, HostTrustError> {
    let mut signer = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|_| HostTrustError::InvalidAssertion)?;
    signer
        .update(canonical_assertion(credential_id, audience, issued_at, nonce, request).as_bytes());
    Ok(hex::encode(signer.finalize().into_bytes()))
}

fn canonical_assertion(
    credential_id: Uuid,
    audience: &str,
    issued_at: DateTime<Utc>,
    nonce: Uuid,
    request: &NormalizedHostContextRequest,
) -> String {
    let mut canonical = String::from("vox-host-assertion-v1");
    for field in [
        credential_id.to_string(),
        audience.to_owned(),
        issued_at.timestamp().to_string(),
        nonce.to_string(),
        request.host_user_id.clone(),
        request
            .organization_external_key
            .clone()
            .unwrap_or_default(),
    ] {
        canonical.push('|');
        canonical.push_str(&field.len().to_string());
        canonical.push(':');
        canonical.push_str(&field);
    }
    canonical
}
