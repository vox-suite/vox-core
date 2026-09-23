use super::AdapterExecutionError;
use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

pub const DEFAULT_MAX_CLOCK_SKEW_SECONDS: i64 = 300; // 5 minutes

pub struct ExtensionIntegrityAssertion {
    pub timestamp: DateTime<Utc>,
    pub nonce: Uuid,
    pub signature: String,
    pub payload_hash: String,
}

#[derive(Clone, Debug)]
pub struct IntegrityVerification<'a> {
    pub secret: &'a [u8],
    pub extension_id: Uuid,
    pub capability_key: &'a str,
    pub payload: &'a [u8],
    pub timestamp: DateTime<Utc>,
    pub nonce: Uuid,
    pub signature: &'a str,
}

pub struct ExtensionIntegritySigner;

impl ExtensionIntegritySigner {
    pub fn compute_payload_hash(payload: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(payload);
        hex::encode(hasher.finalize())
    }

    pub fn canonical_message(
        timestamp: DateTime<Utc>,
        nonce: Uuid,
        extension_id: Uuid,
        capability_key: &str,
        payload_hash: &str,
    ) -> String {
        format!(
            "{}:{}:{}:{}:{}",
            timestamp.timestamp(),
            nonce,
            extension_id,
            capability_key,
            payload_hash
        )
    }

    pub fn sign(
        secret: &[u8],
        extension_id: Uuid,
        capability_key: &str,
        payload: &[u8],
        timestamp: DateTime<Utc>,
        nonce: Uuid,
    ) -> Result<ExtensionIntegrityAssertion, AdapterExecutionError> {
        let payload_hash = Self::compute_payload_hash(payload);
        let canonical = Self::canonical_message(
            timestamp,
            nonce,
            extension_id,
            capability_key,
            &payload_hash,
        );

        let mut mac = HmacSha256::new_from_slice(secret)
            .map_err(|e| AdapterExecutionError::IntegrityError(e.to_string()))?;
        mac.update(canonical.as_bytes());
        let signature = hex::encode(mac.finalize().into_bytes());

        Ok(ExtensionIntegrityAssertion {
            timestamp,
            nonce,
            signature,
            payload_hash,
        })
    }

    pub fn verify(
        req: &IntegrityVerification<'_>,
        now: DateTime<Utc>,
        max_skew_seconds: Option<i64>,
    ) -> Result<(), AdapterExecutionError> {
        let skew = max_skew_seconds.unwrap_or(DEFAULT_MAX_CLOCK_SKEW_SECONDS);
        let diff = (now.timestamp() - req.timestamp.timestamp()).abs();
        if diff > skew {
            return Err(AdapterExecutionError::IntegrityError(format!(
                "assertion timestamp expired or out of skew window: diff={diff}s, max={skew}s"
            )));
        }

        let payload_hash = Self::compute_payload_hash(req.payload);
        let canonical = Self::canonical_message(
            req.timestamp,
            req.nonce,
            req.extension_id,
            req.capability_key,
            &payload_hash,
        );

        let mut mac = HmacSha256::new_from_slice(req.secret)
            .map_err(|e| AdapterExecutionError::IntegrityError(e.to_string()))?;
        mac.update(canonical.as_bytes());

        let sig_bytes = hex::decode(req.signature).map_err(|e| {
            AdapterExecutionError::IntegrityError(format!("invalid hex signature: {e}"))
        })?;

        mac.verify_slice(&sig_bytes).map_err(|_| {
            AdapterExecutionError::IntegrityError("signature verification failed".into())
        })?;

        Ok(())
    }
}
