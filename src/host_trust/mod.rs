/**
* Host machine trust evaluation and client attestation verification.
*/
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
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

mod gsm;
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

#[derive(Clone, Debug)]
#[allow(dead_code)]
struct ConfiguredCredential {
    deployment_id: Uuid,
    host_app_id: Uuid,
    deployment_external_key: String,
    host_app_external_key: String,
    secret: String,
    secret_hash: Vec<u8>,
    allowed_origins: Vec<String>,
    is_active: bool,
}

impl ConfiguredCredential {
    fn from_stored(stored: gsm::StoredCredential) -> Self {
        let secret_hash = Sha256::digest(stored.secret.as_bytes()).to_vec();
        Self {
            deployment_id: stored.deployment_id,
            host_app_id: stored.host_app_id,
            deployment_external_key: stored.deployment_external_key,
            host_app_external_key: stored.host_app_external_key,
            secret: stored.secret,
            secret_hash,
            allowed_origins: stored.allowed_origins,
            is_active: stored.is_active,
        }
    }
}

impl gsm::StoredCredential {
    fn from_configured(id: Uuid, credential: &ConfiguredCredential) -> Self {
        Self {
            credential_id: id,
            deployment_id: credential.deployment_id,
            host_app_id: credential.host_app_id,
            deployment_external_key: credential.deployment_external_key.clone(),
            host_app_external_key: credential.host_app_external_key.clone(),
            secret: credential.secret.clone(),
            allowed_origins: credential.allowed_origins.clone(),
            is_active: credential.is_active,
        }
    }
}

#[derive(Clone)]
pub struct HostTrustService {
    #[allow(dead_code)]
    db: Db,
    identities: IdentityService,
    credentials: Arc<RwLock<HashMap<Uuid, ConfiguredCredential>>>,
    nonces: Arc<RwLock<HashMap<(Uuid, Uuid), DateTime<Utc>>>>,
    secret_resource: Option<String>,
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
    #[error("host credential store is unavailable")]
    CredentialStore,
    #[error("host context resolution failed")]
    Identity(#[from] IdentityError),
}

impl HostTrustService {
    pub fn new(db: Db) -> Self {
        Self {
            identities: IdentityService::new(db.clone()),
            db,
            credentials: Arc::new(RwLock::new(HashMap::new())),
            nonces: Arc::new(RwLock::new(HashMap::new())),
            secret_resource: None,
        }
    }

    pub async fn load_durable_credentials(
        &mut self,
        secret_resource: Option<String>,
    ) -> Result<(), HostTrustError> {
        let Some(resource) = secret_resource.filter(|value| !value.trim().is_empty()) else {
            return Ok(());
        };
        let loaded = gsm::read_credentials(&resource)
            .await
            .map_err(|_| HostTrustError::CredentialStore)?;
        let mut creds = self.credentials.write().unwrap();
        creds.clear();
        for credential in loaded {
            let id = credential.credential_id;
            creds.insert(id, ConfiguredCredential::from_stored(credential));
        }
        drop(creds);
        self.secret_resource = Some(resource);
        Ok(())
    }

    async fn persist_credentials(&self) -> Result<(), HostTrustError> {
        let Some(resource) = &self.secret_resource else {
            return Ok(());
        };
        let stored = {
            let creds = self.credentials.read().unwrap();
            creds
                .iter()
                .map(|(id, credential)| gsm::StoredCredential::from_configured(*id, credential))
                .collect::<Vec<_>>()
        };
        gsm::write_credentials(resource, &stored)
            .await
            .map_err(|_| HostTrustError::CredentialStore)?;
        Ok(())
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

        let deployment_id = Uuid::new_v4();
        let host_app_id = Uuid::new_v4();
        let credential_id = Uuid::new_v4();
        let secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        let secret_hash = Sha256::digest(secret.as_bytes()).to_vec();
        let aud = audience(&deployment_external_key, &host_app_external_key);

        let credential = ConfiguredCredential {
            deployment_id,
            host_app_id,
            deployment_external_key: deployment_external_key.clone(),
            host_app_external_key: host_app_external_key.clone(),
            secret: secret.clone(),
            secret_hash,
            allowed_origins: allowed_origins.clone(),
            is_active: true,
        };

        {
            let mut creds = self.credentials.write().unwrap();
            creds.insert(credential_id, credential);
        }
        if let Err(error) = self.persist_credentials().await {
            self.credentials.write().unwrap().remove(&credential_id);
            return Err(error);
        }

        Ok(RegisteredHostApp {
            deployment_id: DeploymentId(deployment_id),
            host_app_id: HostAppId(host_app_id),
            deployment_external_key,
            host_app_external_key,
            allowed_origins,
            credential: HostAppCredential {
                credential_id,
                audience: aud,
                secret,
            },
        })
    }

    pub async fn rotate_credential(
        &self,
        host_app_id: HostAppId,
    ) -> Result<HostAppCredential, HostTrustError> {
        let (new_cred_id, aud, new_secret) = {
            let mut creds = self.credentials.write().unwrap();
            let old = creds
                .values()
                .find(|c| c.host_app_id == host_app_id.0 && c.is_active)
                .cloned()
                .ok_or(HostTrustError::CredentialNotFound)?;

            for credential in creds.values_mut() {
                if credential.host_app_id == host_app_id.0 {
                    credential.is_active = false;
                }
            }

            let new_cred_id = Uuid::new_v4();
            let new_secret = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
            let secret_hash = Sha256::digest(new_secret.as_bytes()).to_vec();
            let aud = audience(&old.deployment_external_key, &old.host_app_external_key);
            creds.insert(
                new_cred_id,
                ConfiguredCredential {
                    deployment_id: old.deployment_id,
                    host_app_id: old.host_app_id,
                    deployment_external_key: old.deployment_external_key.clone(),
                    host_app_external_key: old.host_app_external_key.clone(),
                    secret: new_secret.clone(),
                    secret_hash,
                    allowed_origins: old.allowed_origins.clone(),
                    is_active: true,
                },
            );
            (new_cred_id, aud, new_secret)
        };
        self.persist_credentials().await?;

        Ok(HostAppCredential {
            credential_id: new_cred_id,
            audience: aud,
            secret: new_secret,
        })
    }

    pub async fn revoke_credential(&self, credential_id: Uuid) -> Result<(), HostTrustError> {
        {
            let mut creds = self.credentials.write().unwrap();
            let credential = creds
                .get_mut(&credential_id)
                .ok_or(HostTrustError::CredentialNotFound)?;
            if !credential.is_active {
                return Err(HostTrustError::CredentialNotFound);
            }
            credential.is_active = false;
        }
        self.persist_credentials().await
    }

    pub async fn resolve_authenticated_context(
        &self,
        assertion: &HostContextAssertion,
        request: &HostContextRequest,
        origin: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<ResolvedUserContext, HostTrustError> {
        let request = NormalizedHostContextRequest::from_request(request)?;

        let cred = {
            let creds = self.credentials.read().unwrap();
            creds
                .get(&assertion.credential_id)
                .cloned()
                .ok_or(HostTrustError::AuthenticationDenied)?
        };

        if !cred.is_active {
            return Err(HostTrustError::AuthenticationDenied);
        }

        let expected_audience = audience(&cred.deployment_external_key, &cred.host_app_external_key);
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
            if !cred.allowed_origins.iter().any(|allowed| allowed == &origin) {
                return Err(HostTrustError::OriginDenied);
            }
        }

        let provided_hash = Sha256::digest(assertion.secret.as_bytes());
        if !bool::from(provided_hash.as_slice().ct_eq(cred.secret_hash.as_slice())) {
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

        {
            let mut nonces = self.nonces.write().unwrap();
            nonces.retain(|_, expires_at| *expires_at >= now);
            let key = (assertion.credential_id, assertion.nonce);
            if nonces.contains_key(&key) {
                return Err(HostTrustError::AssertionReplayed);
            }
            nonces.insert(
                key,
                assertion.issued_at + Duration::seconds(HOST_ASSERTION_MAX_AGE_SECONDS),
            );
        }

        let organization_id = request
            .organization_external_key
            .as_ref()
            .map(|_| HostOrganizationId(Uuid::new_v4()));

        self.identities
            .resolve_context(&UserContextSubject {
                deployment_id: DeploymentId(cred.deployment_id),
                host_app_id: HostAppId(cred.host_app_id),
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
