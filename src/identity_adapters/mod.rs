/**
* Adapters translating external channel identities into core user identities.
*/
use crate::{
    db::Db,
    identity::{DeploymentId, ResolvedUserContext, UserContextId},
};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use ed25519_dalek::{Signature, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

const AUTHENTICATION_SESSION_LIFETIME_SECONDS: i64 = 600;
const FEDERATED_ASSERTION_MAX_LIFETIME_SECONDS: i64 = 600;
const PASSWORDLESS_CHALLENGE_LIFETIME_SECONDS: i64 = 600;
const MAX_IDENTITY_BYTES: usize = 512;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdentityAdapterConfiguration {
    FederatedEd25519 {
        issuer: String,
        audience: String,
        public_key: String,
    },
    PasswordlessRecovery {
        recovery_channel: String,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterIdentityAdapterRequest {
    pub deployment_external_key: String,
    pub external_key: String,
    pub configuration: IdentityAdapterConfiguration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisteredIdentityAdapter {
    pub id: Uuid,
    pub deployment_id: DeploymentId,
    pub external_key: String,
    pub configuration: IdentityAdapterConfiguration,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct FederatedProof {
    pub issuer: String,
    pub audience: String,
    pub subject: String,
    pub issued_at: i64,
    pub expires_at: i64,
    pub nonce: Uuid,
    pub signature: String,
}

impl FederatedProof {
    pub fn sign(
        signing_key: &SigningKey,
        issuer: String,
        audience: String,
        subject: String,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        nonce: Uuid,
    ) -> Result<Self, IdentityAdapterError> {
        let subject = normalize_identity(&subject).ok_or(IdentityAdapterError::InvalidProof)?;
        let canonical = canonical_federated_proof(
            &issuer,
            &audience,
            &subject,
            issued_at.timestamp(),
            expires_at.timestamp(),
            nonce,
        );
        use ed25519_dalek::Signer;
        let signature = signing_key.sign(canonical.as_bytes());
        Ok(Self {
            issuer,
            audience,
            subject,
            issued_at: issued_at.timestamp(),
            expires_at: expires_at.timestamp(),
            nonce,
            signature: hex::encode(signature.to_bytes()),
        })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PasswordlessProof {
    pub challenge_id: Uuid,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", content = "proof", rename_all = "snake_case")]
pub enum IdentityProof {
    Federated(FederatedProof),
    Passwordless(PasswordlessProof),
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthenticateIdentityRequest {
    pub adapter_external_key: String,
    pub proof: IdentityProof,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct AuthenticationResult {
    pub user_context_id: UserContextId,
    pub adapter_external_key: String,
    pub expires_at: DateTime<Utc>,
    pub authentication_token: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StartPasswordlessRecoveryRequest {
    pub adapter_external_key: String,
    pub recovery_handle: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PasswordlessRecoveryStarted {
    pub challenge_id: Uuid,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone, Deserialize, Eq, PartialEq, Serialize)]
pub struct IdentityLinkResult {
    pub link_id: Uuid,
}

pub struct RecoveryDispatch {
    pub challenge_id: Uuid,
    pub recovery_channel: String,
    pub recovery_handle: String,
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[async_trait]
pub trait RecoveryDelivery: Send + Sync {
    async fn deliver(&self, dispatch: RecoveryDispatch) -> Result<(), RecoveryDeliveryError>;
}

#[derive(Debug, thiserror::Error)]
pub enum RecoveryDeliveryError {
    #[error("passwordless recovery delivery is unavailable")]
    Unavailable,
    #[error("passwordless recovery delivery failed")]
    Failed,
}

pub struct UnavailableRecoveryDelivery;

#[async_trait]
impl RecoveryDelivery for UnavailableRecoveryDelivery {
    async fn deliver(&self, _: RecoveryDispatch) -> Result<(), RecoveryDeliveryError> {
        Err(RecoveryDeliveryError::Unavailable)
    }
}

#[derive(Default)]
pub struct RecordingRecoveryDelivery {
    codes: Mutex<HashMap<Uuid, String>>,
}

impl RecordingRecoveryDelivery {
    pub fn code_for(&self, challenge_id: Uuid) -> Option<String> {
        self.codes.lock().ok()?.get(&challenge_id).cloned()
    }
}

#[async_trait]
impl RecoveryDelivery for RecordingRecoveryDelivery {
    async fn deliver(&self, dispatch: RecoveryDispatch) -> Result<(), RecoveryDeliveryError> {
        self.codes
            .lock()
            .map_err(|_| RecoveryDeliveryError::Failed)?
            .insert(dispatch.challenge_id, dispatch.code);
        Ok(())
    }
}

#[derive(Clone)]
pub struct IdentityAdapterService {
    db: Db,
    recovery_delivery: Arc<dyn RecoveryDelivery>,
}

#[derive(Debug, thiserror::Error)]
pub enum IdentityAdapterError {
    #[error("identity adapter registration is invalid")]
    InvalidRegistration,
    #[error("identity proof is invalid")]
    InvalidProof,
    #[error("identity adapter is unavailable")]
    AdapterUnavailable,
    #[error("identity proof is expired")]
    ProofExpired,
    #[error("identity proof has already been used")]
    ProofReplayed,
    #[error("passwordless recovery delivery is unavailable")]
    RecoveryUnavailable,
    #[error("identity authentication is invalid")]
    AuthenticationDenied,
    #[error("identity link is unavailable")]
    LinkNotFound,
    #[error("identity linking requires two distinct verified identities")]
    SameIdentity,
    #[error("identity storage is unavailable")]
    Database(#[from] sqlx::Error),
}

impl IdentityAdapterService {
    pub fn new(db: Db, recovery_delivery: Arc<dyn RecoveryDelivery>) -> Self {
        Self {
            db,
            recovery_delivery,
        }
    }

    pub fn unavailable(db: Db) -> Self {
        Self::new(db, Arc::new(UnavailableRecoveryDelivery))
    }

    pub async fn register_adapter(
        &self,
        request: RegisterIdentityAdapterRequest,
    ) -> Result<RegisteredIdentityAdapter, IdentityAdapterError> {
        let deployment_external_key = normalize_key(&request.deployment_external_key)
            .ok_or(IdentityAdapterError::InvalidRegistration)?;
        let external_key = normalize_key(&request.external_key)
            .ok_or(IdentityAdapterError::InvalidRegistration)?;
        validate_configuration(&request.configuration)?;
        let _ = (deployment_external_key, external_key);
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    pub async fn start_passwordless_recovery(
        &self,
        context: &ResolvedUserContext,
        request: StartPasswordlessRecoveryRequest,
        now: DateTime<Utc>,
    ) -> Result<PasswordlessRecoveryStarted, IdentityAdapterError> {
        let _ = normalize_identity(&request.recovery_handle).ok_or(IdentityAdapterError::InvalidProof)?;
        let _ = normalize_key(&request.adapter_external_key).ok_or(IdentityAdapterError::InvalidProof)?;
        let _ = (context, now);
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    pub async fn authenticate(
        &self,
        context: &ResolvedUserContext,
        request: AuthenticateIdentityRequest,
        now: DateTime<Utc>,
    ) -> Result<AuthenticationResult, IdentityAdapterError> {
        let _ = normalize_key(&request.adapter_external_key).ok_or(IdentityAdapterError::InvalidProof)?;
        let _ = (context, now, request.proof);
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    pub async fn link_identities(
        &self,
        source_authentication_token: &str,
        target_authentication_token: &str,
        now: DateTime<Utc>,
    ) -> Result<IdentityLinkResult, IdentityAdapterError> {
        let _ = (
            normalize_identity(source_authentication_token),
            normalize_identity(target_authentication_token),
            now,
        );
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    pub async fn unlink_identities(
        &self,
        source_authentication_token: &str,
        target_authentication_token: &str,
        now: DateTime<Utc>,
    ) -> Result<(), IdentityAdapterError> {
        let _ = (
            normalize_identity(source_authentication_token),
            normalize_identity(target_authentication_token),
            now,
        );
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    async fn load_adapter(
        &self,
        _context: &ResolvedUserContext,
        external_key: &str,
    ) -> Result<LoadedAdapter, IdentityAdapterError> {
        let _ = normalize_key(external_key).ok_or(IdentityAdapterError::InvalidProof)?;
        Err(IdentityAdapterError::AdapterUnavailable)
    }

    async fn verify_federated(
        &self,
        adapter_id: Uuid,
        issuer: &str,
        audience: &str,
        public_key: &str,
        proof: FederatedProof,
        now: DateTime<Utc>,
    ) -> Result<Vec<u8>, IdentityAdapterError> {
        let subject =
            normalize_identity(&proof.subject).ok_or(IdentityAdapterError::InvalidProof)?;
        if proof.issuer != issuer || proof.audience != audience {
            return Err(IdentityAdapterError::InvalidProof);
        }
        let issued_at = DateTime::from_timestamp(proof.issued_at, 0)
            .ok_or(IdentityAdapterError::InvalidProof)?;
        let expires_at = DateTime::from_timestamp(proof.expires_at, 0)
            .ok_or(IdentityAdapterError::InvalidProof)?;
        if issued_at > now + Duration::seconds(60)
            || expires_at <= now
            || expires_at - issued_at > Duration::seconds(FEDERATED_ASSERTION_MAX_LIFETIME_SECONDS)
        {
            return Err(IdentityAdapterError::ProofExpired);
        }
        let public_key = decode_public_key(public_key)?;
        let signature =
            hex::decode(&proof.signature).map_err(|_| IdentityAdapterError::InvalidProof)?;
        let signature =
            Signature::from_slice(&signature).map_err(|_| IdentityAdapterError::InvalidProof)?;
        public_key
            .verify(
                canonical_federated_proof(
                    issuer,
                    audience,
                    &subject,
                    proof.issued_at,
                    proof.expires_at,
                    proof.nonce,
                )
                .as_bytes(),
                &signature,
            )
            .map_err(|_| IdentityAdapterError::InvalidProof)?;
        sqlx::query("DELETE FROM federated_identity_nonces WHERE expires_at < $1")
            .bind(now)
            .execute(self.db.pool())
            .await?;
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO federated_identity_nonces (adapter_id, nonce, expires_at) \
             VALUES ($1, $2, $3) ON CONFLICT DO NOTHING RETURNING nonce",
        )
        .bind(adapter_id)
        .bind(proof.nonce)
        .bind(expires_at)
        .fetch_optional(self.db.pool())
        .await?;
        if inserted.is_none() {
            return Err(IdentityAdapterError::ProofReplayed);
        }
        Ok(hash(&subject))
    }

    async fn verify_passwordless(
        &self,
        adapter_id: Uuid,
        context_id: UserContextId,
        proof: PasswordlessProof,
        now: DateTime<Utc>,
    ) -> Result<Vec<u8>, IdentityAdapterError> {
        let handle_hash = sqlx::query_scalar::<_, Vec<u8>>(
            "UPDATE passwordless_recovery_challenges SET consumed_at = $5 \
             WHERE id = $1 AND adapter_id = $2 AND user_context_id = $3 \
               AND code_hash = $4 AND consumed_at IS NULL AND expires_at > $5 \
             RETURNING recovery_handle_hash",
        )
        .bind(proof.challenge_id)
        .bind(adapter_id)
        .bind(context_id.0)
        .bind(hash(&proof.code))
        .bind(now)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(IdentityAdapterError::AuthenticationDenied)?;
        Ok(handle_hash)
    }
}

struct LoadedAdapter {
    id: Uuid,
    external_key: String,
    configuration: IdentityAdapterConfiguration,
}

async fn consume_authentication(
    tx: &mut Transaction<'_, Postgres>,
    token: &str,
    now: DateTime<Utc>,
) -> Result<Uuid, IdentityAdapterError> {
    let token = normalize_identity(token).ok_or(IdentityAdapterError::AuthenticationDenied)?;
    sqlx::query_scalar::<_, Uuid>(
        "UPDATE identity_authentication_sessions SET consumed_at = $2 \
         WHERE token_hash = $1 AND consumed_at IS NULL AND expires_at > $2 \
         RETURNING login_identity_id",
    )
    .bind(hash(&token))
    .bind(now)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(IdentityAdapterError::AuthenticationDenied)
}

fn ordered_pair(left: Uuid, right: Uuid) -> Result<(Uuid, Uuid), IdentityAdapterError> {
    if left == right {
        return Err(IdentityAdapterError::SameIdentity);
    }
    Ok(if left < right {
        (left, right)
    } else {
        (right, left)
    })
}

fn adapter_kind(configuration: &IdentityAdapterConfiguration) -> &'static str {
    match configuration {
        IdentityAdapterConfiguration::FederatedEd25519 { .. } => "federated_ed25519",
        IdentityAdapterConfiguration::PasswordlessRecovery { .. } => "passwordless_recovery",
    }
}

fn validate_configuration(
    configuration: &IdentityAdapterConfiguration,
) -> Result<(), IdentityAdapterError> {
    match configuration {
        IdentityAdapterConfiguration::FederatedEd25519 {
            issuer,
            audience,
            public_key,
        } => {
            if normalize_identity(issuer).is_none() || normalize_identity(audience).is_none() {
                return Err(IdentityAdapterError::InvalidRegistration);
            }
            decode_public_key(public_key).map(|_| ())
        }
        IdentityAdapterConfiguration::PasswordlessRecovery { recovery_channel } => {
            normalize_key(recovery_channel)
                .map(|_| ())
                .ok_or(IdentityAdapterError::InvalidRegistration)
        }
    }
}

fn decode_public_key(value: &str) -> Result<VerifyingKey, IdentityAdapterError> {
    let bytes = hex::decode(value).map_err(|_| IdentityAdapterError::InvalidRegistration)?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| IdentityAdapterError::InvalidRegistration)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| IdentityAdapterError::InvalidRegistration)
}

fn normalize_key(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 255 {
        None
    } else {
        Some(value.to_owned())
    }
}

fn normalize_identity(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_IDENTITY_BYTES {
        None
    } else {
        Some(value.to_owned())
    }
}

fn hash(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}

fn opaque_token() -> String {
    format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple())
}

fn canonical_federated_proof(
    issuer: &str,
    audience: &str,
    subject: &str,
    issued_at: i64,
    expires_at: i64,
    nonce: Uuid,
) -> String {
    let mut output = String::from("vox-federated-identity-v1");
    for field in [
        issuer.to_owned(),
        audience.to_owned(),
        subject.to_owned(),
        issued_at.to_string(),
        expires_at.to_string(),
        nonce.to_string(),
    ] {
        output.push('|');
        output.push_str(&field.len().to_string());
        output.push(':');
        output.push_str(&field);
    }
    output
}
