use crate::{
    db::Db,
    identity::{IdentityError, IdentityService, UserId},
};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct VoiceSignature {
    pub features: Vec<f32>,
    #[serde(default)]
    pub sample_count: usize,
    #[serde(default)]
    pub sample_duration_ms: u64,
    #[serde(default)]
    pub model: Option<String>,
}

impl VoiceSignature {
    pub fn new(features: Vec<f32>) -> Self {
        Self {
            sample_count: 1,
            sample_duration_ms: 0,
            model: Some("vox-v1".into()),
            features,
        }
    }

    /// Computes cosine similarity between two voice signatures in [-1.0, 1.0].
    pub fn cosine_similarity(&self, other: &Self) -> f64 {
        if self.features.is_empty()
            || other.features.is_empty()
            || self.features.len() != other.features.len()
        {
            return 0.0;
        }

        let mut dot = 0.0f32;
        let mut norm_a = 0.0f32;
        let mut norm_b = 0.0f32;

        for (a, b) in self.features.iter().zip(&other.features) {
            dot += a * b;
            norm_a += a * a;
            norm_b += b * b;
        }

        let denom = norm_a.sqrt() * norm_b.sqrt();
        if denom == 0.0 {
            return 0.0;
        }

        (dot / denom) as f64
    }

    /// Parses a raw voice signature string representation (JSON object, JSON array, or comma-separated floats).
    pub fn from_raw(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }

        // 1. JSON object with `features` field
        if let Ok(sig) = serde_json::from_str::<VoiceSignature>(trimmed)
            && !sig.features.is_empty()
        {
            return Some(sig);
        }

        // 2. JSON array of floats `[0.1, 0.2, ...]`
        if let Ok(features) = serde_json::from_str::<Vec<f32>>(trimmed)
            && !features.is_empty()
        {
            return Some(Self::new(features));
        }

        // 3. Comma-separated floats `0.1, 0.2, ...`
        let parsed: Vec<f32> = trimmed
            .split(',')
            .filter_map(|s| s.trim().parse::<f32>().ok())
            .collect();

        if !parsed.is_empty() {
            return Some(Self::new(parsed));
        }

        None
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| "{}".to_string())
    }
}

#[derive(Clone)]
pub struct VoiceprintService {
    db: Db,
}

impl VoiceprintService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Retrieves the stored voice signature for a user if enrolled.
    pub async fn get_voiceprint(
        &self,
        user_id: UserId,
    ) -> Result<Option<VoiceSignature>, sqlx::Error> {
        let row = sqlx::query("SELECT signature FROM user_voiceprints WHERE user_id = $1")
            .bind(user_id.0)
            .fetch_optional(self.db.pool())
            .await?;

        if let Some(r) = row {
            let val: serde_json::Value = r.get("signature");
            if let Ok(sig) = serde_json::from_value::<VoiceSignature>(val) {
                return Ok(Some(sig));
            }
        }

        Ok(None)
    }

    /// Saves or updates the enrolled voice signature for a user.
    pub async fn save_voiceprint(
        &self,
        user_id: UserId,
        signature: &VoiceSignature,
        sample_duration_ms: i32,
    ) -> Result<(), sqlx::Error> {
        let sig_json = serde_json::to_value(signature)
            .map_err(|e| sqlx::Error::Protocol(format!("failed to serialize signature: {e}")))?;

        sqlx::query(
            "INSERT INTO user_voiceprints (user_id, signature, sample_duration_ms, created_at, updated_at) \
             VALUES ($1, $2, $3, now(), now()) \
             ON CONFLICT (user_id) DO UPDATE SET \
             signature = EXCLUDED.signature, \
             sample_duration_ms = EXCLUDED.sample_duration_ms, \
             updated_at = now()",
        )
        .bind(user_id.0)
        .bind(sig_json)
        .bind(sample_duration_ms)
        .execute(self.db.pool())
        .await?;

        Ok(())
    }

    /// Finds an existing user ID by their registered profile name (case-insensitive).
    pub async fn find_user_by_name(&self, name: &str) -> Result<Option<UserId>, sqlx::Error> {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }

        let user_id = sqlx::query_scalar::<_, Uuid>(
            "SELECT user_id FROM user_profiles \
             WHERE LOWER(facts->>'name') = LOWER($1) \
             LIMIT 1",
        )
        .bind(trimmed)
        .fetch_optional(self.db.pool())
        .await?;

        Ok(user_id.map(UserId))
    }

    /// Retrieves all phone numbers associated with a user.
    pub async fn get_user_phones(&self, user_id: UserId) -> Result<Vec<String>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT external_id FROM user_identities \
             WHERE user_id = $1 AND channel = 'phone'",
        )
        .bind(user_id.0)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows.into_iter().map(|r| r.get("external_id")).collect())
    }

    /// Creates a fresh user profile with the given name.
    pub async fn create_user_with_name(&self, name: &str) -> Result<UserId, sqlx::Error> {
        let mut tx = self.db.pool().begin().await?;
        let user_id =
            sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
                .fetch_one(&mut *tx)
                .await?;

        sqlx::query(
            "INSERT INTO user_profiles (user_id, facts, version, updated_at) \
             VALUES ($1, jsonb_build_object('name', $2::text), 1, now())",
        )
        .bind(user_id)
        .bind(name.trim())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        let user_id = UserId(user_id);
        IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(identity_storage_error)?;
        Ok(user_id)
    }

    /// Updates the active user of a conversation session when identity switches during a call.
    pub async fn update_conversation_user(
        &self,
        conversation_id: Uuid,
        user_id: UserId,
    ) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE conversations SET active_user_id = $1 WHERE id = $2")
            .bind(user_id.0)
            .bind(conversation_id)
            .execute(self.db.pool())
            .await?;

        Ok(())
    }
}

fn identity_storage_error(error: IdentityError) -> sqlx::Error {
    match error {
        IdentityError::Database(error) => error,
        other => sqlx::Error::Protocol(other.to_string()),
    }
}

/// Normalizes and extracts numeric digits from speech text (e.g. "+91 98765-43210" -> "919876543210").
pub fn extract_phone_digits(text: &str) -> String {
    text.chars().filter(|c| c.is_ascii_digit()).collect()
}

/// Verifies if spoken phone digits match a registered phone number.
/// Handles varying country codes and local suffixes (e.g., "9876543210" matching "+919876543210").
pub fn verify_phone_match(spoken: &str, registered: &str) -> bool {
    let s_digits = extract_phone_digits(spoken);
    let r_digits = extract_phone_digits(registered);

    if s_digits.len() < 10 || r_digits.len() < 10 {
        return false;
    }

    // Direct equality
    if s_digits == r_digits {
        return true;
    }

    // Suffix match (e.g. 10-digit phone spoken, registered has country code)
    if s_digits.len() >= 10 && r_digits.ends_with(&s_digits) {
        return true;
    }

    if r_digits.len() >= 10 && s_digits.ends_with(&r_digits) {
        return true;
    }

    false
}

impl VoiceSignature {
    pub fn usable(&self) -> bool {
        self.sample_duration_ms >= 1000
            && self.model.as_deref().is_some_and(|model| {
                model.strip_prefix("onnx-sha256:").is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
            })
            && !self.features.is_empty()
            && self.features.iter().all(|v| v.is_finite())
            && self
                .features
                .iter()
                .map(|v| (*v as f64).powi(2))
                .sum::<f64>()
                > 0.0
    }

    pub fn comparable(&self, other: &Self) -> bool {
        self.usable()
            && other.usable()
            && self.model == other.model
            && self.features.len() == other.features.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity() {
        let a = VoiceSignature::new(vec![1.0, 0.0, 0.0]);
        let b = VoiceSignature::new(vec![1.0, 0.0, 0.0]);
        assert!((a.cosine_similarity(&b) - 1.0).abs() < 1e-6);

        let orthogonal = VoiceSignature::new(vec![0.0, 1.0, 0.0]);
        assert!((a.cosine_similarity(&orthogonal) - 0.0).abs() < 1e-6);

        let opposite = VoiceSignature::new(vec![-1.0, 0.0, 0.0]);
        assert!((a.cosine_similarity(&opposite) - (-1.0)).abs() < 1e-6);
    }

    #[test]
    fn test_from_raw_formats() {
        // JSON struct
        let raw_json = r#"{"features": [0.1, 0.2, 0.3], "sample_count": 2, "model": "test"}"#;
        let sig = VoiceSignature::from_raw(raw_json).unwrap();
        assert_eq!(sig.features, vec![0.1, 0.2, 0.3]);
        assert_eq!(sig.sample_count, 2);

        // JSON array
        let raw_arr = r#"[0.4, 0.5, 0.6]"#;
        let sig2 = VoiceSignature::from_raw(raw_arr).unwrap();
        assert_eq!(sig2.features, vec![0.4, 0.5, 0.6]);

        // Comma-separated
        let raw_csv = "0.7, 0.8, 0.9";
        let sig3 = VoiceSignature::from_raw(raw_csv).unwrap();
        assert_eq!(sig3.features, vec![0.7, 0.8, 0.9]);

        // Empty / invalid
        assert_eq!(VoiceSignature::from_raw(""), None);
        assert_eq!(VoiceSignature::from_raw("invalid"), None);
    }

    #[test]
    fn test_phone_verification_matching() {
        assert!(verify_phone_match("9876543210", "+919876543210"));
        assert!(verify_phone_match("+91 98765 43210", "9876543210"));
        assert!(verify_phone_match(
            "My number is 9876543210.",
            "+919876543210"
        ));
        assert!(!verify_phone_match("1234567890", "+919876543210"));
        assert!(!verify_phone_match("", "+919876543210"));
    }
}

#[cfg(test)]
mod quality_tests {
    use super::*;
    #[test]
    fn only_real_compatible_sufficient_samples_are_comparable() {
        let mut signature = VoiceSignature::new(vec![1.0, 0.5]);
        assert!(!signature.usable());
        signature.model = Some(format!("onnx-sha256:{}", "a".repeat(64)));
        signature.sample_duration_ms = 999;
        assert!(!signature.usable());
        signature.sample_duration_ms = 1000;
        assert!(signature.comparable(&signature));
        let mut other = signature.clone();
        other.model = Some(format!("onnx-sha256:{}", "b".repeat(64)));
        assert!(!signature.comparable(&other));
        other = signature.clone();
        other.features[0] = f32::NAN;
        assert!(!other.usable());
        other.features = vec![0.0, 0.0];
        assert!(!other.usable());
    }
}
