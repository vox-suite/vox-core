use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const CODE_TTL_MINUTES: i64 = 10;
const MAX_ATTEMPTS: i32 = 5;
const MAX_SENDS_PER_PHONE_PER_HOUR: i64 = 3;
const MAX_SENDS_PER_USER_PER_DAY: i64 = 10;

#[derive(Debug, thiserror::Error)]
pub enum PhoneVerificationError {
    #[error("phone verification storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("phone number is not linked to this account")]
    NotLinked,
    #[error("too many verification codes requested")]
    RateLimited,
    #[error("verification code is invalid or expired")]
    InvalidCode,
}

pub struct IssuedCode {
    pub code: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct PhoneVerificationService {
    pool: PgPool,
}

fn hash_code(id: Uuid, code: &str) -> String {
    hex::encode(Sha256::digest(format!("{id}:{code}")))
}

impl PhoneVerificationService {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn issue(
        &self,
        user_id: Uuid,
        phone_digits: &str,
    ) -> Result<IssuedCode, PhoneVerificationError> {
        let linked = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channel_identities \
             WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2 \
               AND revoked_at IS NULL)",
        )
        .bind(user_id)
        .bind(phone_digits)
        .fetch_one(&self.pool)
        .await?;
        if !linked {
            return Err(PhoneVerificationError::NotLinked);
        }

        let (per_phone, per_user) = sqlx::query_as::<_, (i64, i64)>(
            "SELECT \
                COUNT(*) FILTER (WHERE normalized_phone = $2 AND created_at > now() - interval '1 hour'), \
                COUNT(*) FILTER (WHERE user_id = $1 AND created_at > now() - interval '1 day') \
             FROM phone_verifications WHERE user_id = $1 OR normalized_phone = $2",
        )
        .bind(user_id)
        .bind(phone_digits)
        .fetch_one(&self.pool)
        .await?;
        if per_phone >= MAX_SENDS_PER_PHONE_PER_HOUR || per_user >= MAX_SENDS_PER_USER_PER_DAY {
            return Err(PhoneVerificationError::RateLimited);
        }

        let id = Uuid::new_v4();
        let code = format!("{:06}", Uuid::new_v4().as_u128() % 1_000_000);
        let expires_at = Utc::now() + Duration::minutes(CODE_TTL_MINUTES);
        sqlx::query(
            "INSERT INTO phone_verifications (id, user_id, normalized_phone, code_hash, expires_at) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(id)
        .bind(user_id)
        .bind(phone_digits)
        .bind(hash_code(id, &code))
        .bind(expires_at)
        .execute(&self.pool)
        .await?;
        Ok(IssuedCode { code, expires_at })
    }

    pub async fn confirm(
        &self,
        user_id: Uuid,
        phone_digits: &str,
        code: &str,
    ) -> Result<(), PhoneVerificationError> {
        let mut tx = self.pool.begin().await?;
        let row = sqlx::query(
            "SELECT id, code_hash FROM phone_verifications \
             WHERE user_id = $1 AND normalized_phone = $2 AND consumed_at IS NULL \
               AND expires_at > now() AND attempts < $3 \
             ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
        )
        .bind(user_id)
        .bind(phone_digits)
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(PhoneVerificationError::InvalidCode)?;
        let id: Uuid = row.get("id");
        let stored: String = row.get("code_hash");

        sqlx::query("UPDATE phone_verifications SET attempts = attempts + 1 WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;

        let matches: bool = hash_code(id, code.trim())
            .as_bytes()
            .ct_eq(stored.as_bytes())
            .into();
        if !matches {
            tx.commit().await?;
            return Err(PhoneVerificationError::InvalidCode);
        }

        sqlx::query("UPDATE phone_verifications SET consumed_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        let updated = sqlx::query(
            "UPDATE channel_identities SET otp_verified_at = now() \
             WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2 \
               AND revoked_at IS NULL",
        )
        .bind(user_id)
        .bind(phone_digits)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(PhoneVerificationError::NotLinked);
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn verified_phone(&self, user_id: Uuid) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT normalized_external_id FROM channel_identities \
             WHERE user_id = $1 AND channel = 'phone' AND revoked_at IS NULL \
               AND otp_verified_at IS NOT NULL \
             ORDER BY otp_verified_at DESC LIMIT 1",
        )
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await
    }
}
