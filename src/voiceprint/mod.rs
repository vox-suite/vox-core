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
    pub model: Option<String>,
}

impl VoiceSignature {
    pub fn new(features: Vec<f32>) -> Self {
        Self {
            sample_count: 1,
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
        let owner = IdentityService::new(self.db.clone())
            .owner_for_user(user_id)
            .await
            .map_err(identity_storage_error)?;
        sqlx::query("UPDATE conversations SET user_id = $1, user_context_id = $2 WHERE id = $3")
            .bind(user_id.0)
            .bind(owner.user_context_id.0)
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

    if s_digits.is_empty() || r_digits.is_empty() {
        return false;
    }

    // Direct equality
    if s_digits == r_digits {
        return true;
    }

    // Suffix match (e.g. 10-digit phone spoken, registered has country code)
    if s_digits.len() >= 7 && r_digits.ends_with(&s_digits) {
        return true;
    }

    if r_digits.len() >= 7 && s_digits.ends_with(&r_digits) {
        return true;
    }

    false
}

/// Verifies whether an incoming voice matches the registered user's voice signature.
/// Uses Jev System 1 evaluation if configured, or falls back to standard cosine similarity threshold (>= 0.75).
pub async fn verify_voice_match_with_jev(
    jev: Option<&crate::jev::JevClient>,
    similarity: f64,
    registered_name: &str,
) -> bool {
    // 1. High-confidence heuristic bounds
    if similarity >= 0.85 {
        return true;
    }
    if similarity < 0.60 {
        return false;
    }

    // 2. Ambiguous similarity region (0.60 .. 0.85): Use Jev System 1 choice evaluation if available
    if let Some(client) = jev {
        let state = serde_json::json!({
            "cosine_similarity": similarity,
            "registered_user": registered_name,
            "decision_context": "Voice biometrics identity verification for incoming telephony call"
        });

        if let Ok((choice, confidence, _)) = client
            .choice(
                state,
                "Determine if the incoming voice sample matches the registered user profile.",
                &[
                    ("match", Some("Acoustic features match the registered speaker with acceptable biometric tolerance")),
                    ("mismatch", Some("Acoustic features indicate a different or distinct speaker")),
                ],
            )
            .await
            && confidence >= 0.70 {
                return choice == "match";
            }
    }

    // 3. Fallback threshold
    similarity >= 0.75
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
