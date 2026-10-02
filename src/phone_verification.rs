use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use subtle::ConstantTimeEq;
use uuid::Uuid;

const GUEST_DEPLOYMENT: &str = "vox.standalone.deployment";
const GUEST_HOST: &str = "vox.standalone.bridge";
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
    #[error("phone number is already verified by another account")]
    Conflict,
}

#[derive(Debug, Default)]
pub struct ConfirmOutcome {
    pub merged_users: Vec<Uuid>,
}

#[derive(Debug, Eq, PartialEq)]
pub enum LinkState {
    Pending,
    AlreadyVerified,
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

    async fn verified_by_other(
        executor: impl sqlx::PgExecutor<'_>,
        user_id: Uuid,
        phone_digits: &str,
    ) -> Result<bool, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM channel_identities \
             WHERE channel = 'phone' AND normalized_external_id = $2 AND revoked_at IS NULL \
               AND otp_verified_at IS NOT NULL AND user_id <> $1)",
        )
        .bind(user_id)
        .bind(phone_digits)
        .fetch_one(executor)
        .await
    }

    pub async fn begin_link(
        &self,
        user_id: Uuid,
        phone_digits: &str,
    ) -> Result<LinkState, PhoneVerificationError> {
        if Self::verified_by_other(&self.pool, user_id, phone_digits).await? {
            return Err(PhoneVerificationError::Conflict);
        }
        let already_verified = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channel_identities \
             WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2 \
               AND revoked_at IS NULL AND otp_verified_at IS NOT NULL)",
        )
        .bind(user_id)
        .bind(phone_digits)
        .fetch_one(&self.pool)
        .await?;
        if already_verified {
            return Ok(LinkState::AlreadyVerified);
        }
        sqlx::query(
            "INSERT INTO pending_phone_links (user_id, normalized_phone) VALUES ($1, $2) \
             ON CONFLICT (user_id) DO UPDATE \
             SET normalized_phone = EXCLUDED.normalized_phone, created_at = now()",
        )
        .bind(user_id)
        .bind(phone_digits)
        .execute(&self.pool)
        .await?;
        Ok(LinkState::Pending)
    }

    pub async fn pending_phone(&self, user_id: Uuid) -> Result<Option<String>, sqlx::Error> {
        sqlx::query_scalar("SELECT normalized_phone FROM pending_phone_links WHERE user_id = $1")
            .bind(user_id)
            .fetch_optional(&self.pool)
            .await
    }

    pub async fn issue(
        &self,
        user_id: Uuid,
        phone_digits: &str,
    ) -> Result<IssuedCode, PhoneVerificationError> {
        let claimed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channel_identities \
                 WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2 \
                   AND revoked_at IS NULL) \
                OR EXISTS(SELECT 1 FROM pending_phone_links \
                 WHERE user_id = $1 AND normalized_phone = $2)",
        )
        .bind(user_id)
        .bind(phone_digits)
        .fetch_one(&self.pool)
        .await?;
        if !claimed {
            return Err(PhoneVerificationError::NotLinked);
        }
        if Self::verified_by_other(&self.pool, user_id, phone_digits).await? {
            return Err(PhoneVerificationError::Conflict);
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
    ) -> Result<ConfirmOutcome, PhoneVerificationError> {
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

        let owners = sqlx::query_as::<_, (Uuid, bool)>(
            "SELECT user_id, otp_verified_at IS NOT NULL FROM channel_identities \
             WHERE channel = 'phone' AND normalized_external_id = $1 AND revoked_at IS NULL \
             FOR UPDATE",
        )
        .bind(phone_digits)
        .fetch_all(&mut *tx)
        .await?;
        for (owner, verified) in owners {
            if owner == user_id {
                continue;
            }
            if verified {
                return Err(PhoneVerificationError::Conflict);
            }
            sqlx::query(
                "DELETE FROM channel_identities \
                 WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2",
            )
            .bind(owner)
            .bind(phone_digits)
            .execute(&mut *tx)
            .await?;
        }

        let guests = sqlx::query_scalar::<_, Uuid>(
            "SELECT DISTINCT c.user_id FROM user_contexts c \
             JOIN host_apps h ON h.id = c.host_app_id \
             JOIN platform_deployments d ON d.id = h.deployment_id \
             WHERE d.external_key = $1 AND h.external_key = $2 \
               AND ltrim(c.host_user_id, '+') = $3 AND c.user_id <> $4",
        )
        .bind(GUEST_DEPLOYMENT)
        .bind(GUEST_HOST)
        .bind(phone_digits)
        .bind(user_id)
        .fetch_all(&mut *tx)
        .await?;
        for guest in &guests {
            sqlx::query("SELECT merge_user_accounts($1, $2)")
                .bind(guest)
                .bind(user_id)
                .execute(&mut *tx)
                .await?;
        }

        let updated = sqlx::query(
            "UPDATE channel_identities \
             SET otp_verified_at = now(), verified_at = COALESCE(verified_at, now()) \
             WHERE user_id = $1 AND channel = 'phone' AND normalized_external_id = $2 \
               AND revoked_at IS NULL",
        )
        .bind(user_id)
        .bind(phone_digits)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            sqlx::query(
                "INSERT INTO channel_identities \
                 (user_id, channel, normalized_external_id, verified_at, otp_verified_at) \
                 VALUES ($1, 'phone', $2, now(), now())",
            )
            .bind(user_id)
            .bind(phone_digits)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query("DELETE FROM pending_phone_links WHERE user_id = $1")
            .bind(user_id)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(ConfirmOutcome {
            merged_users: guests,
        })
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
