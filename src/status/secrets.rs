use super::{StatusError, WebhookSecretStore};
use crate::db::Db;
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use uuid::Uuid;

/// Encrypts subscription signing keys before storing them in PostgreSQL. The
/// deployment supplies the same independent 256-bit key to API and worker.
#[derive(Clone)]
pub struct EncryptedWebhookSecretStore {
    db: Db,
    key: [u8; 32],
}

impl EncryptedWebhookSecretStore {
    pub fn from_hex_key(db: Db, value: &str) -> Result<Self, StatusError> {
        let bytes = hex::decode(value).map_err(|_| StatusError::Invalid)?;
        let key: [u8; 32] = bytes.try_into().map_err(|_| StatusError::Invalid)?;
        Ok(Self { db, key })
    }

    fn cipher(&self) -> Result<LessSafeKey, StatusError> {
        let key =
            UnboundKey::new(&aead::AES_256_GCM, &self.key).map_err(|_| StatusError::Unavailable)?;
        Ok(LessSafeKey::new(key))
    }

    fn decrypt(&self, id: Uuid, stored: &[u8]) -> Result<String, StatusError> {
        let (nonce, body) = stored
            .split_at_checked(12)
            .ok_or(StatusError::Unavailable)?;
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| StatusError::Unavailable)?;
        let mut plaintext = body.to_vec();
        let opened = self
            .cipher()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(id.as_bytes().as_slice()),
                &mut plaintext,
            )
            .map_err(|_| StatusError::Unavailable)?;
        String::from_utf8(opened.to_vec()).map_err(|_| StatusError::Unavailable)
    }
}

#[async_trait::async_trait]
impl WebhookSecretStore for EncryptedWebhookSecretStore {
    async fn put_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
        secret: String,
    ) -> Result<(), StatusError> {
        let mut nonce_bytes = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce_bytes)
            .map_err(|_| StatusError::Unavailable)?;
        let mut ciphertext = secret.into_bytes();
        self.cipher()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce_bytes),
                Aad::from(id.as_bytes().as_slice()),
                &mut ciphertext,
            )
            .map_err(|_| StatusError::Unavailable)?;
        let mut stored = Vec::with_capacity(12 + ciphertext.len());
        stored.extend_from_slice(&nonce_bytes);
        stored.extend_from_slice(&ciphertext);
        sqlx::query(
            "INSERT INTO status_webhook_secrets (subscription_id, ciphertext)
             VALUES ($1, $2) ON CONFLICT (subscription_id) DO UPDATE
             SET ciphertext=EXCLUDED.ciphertext, updated_at=now()",
        )
        .bind(id)
        .bind(stored)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    async fn get(&self, id: Uuid) -> Result<String, StatusError> {
        let stored: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT ciphertext FROM status_webhook_secrets WHERE subscription_id=$1",
        )
        .bind(id)
        .fetch_optional(self.db.pool())
        .await?;
        let stored = stored.ok_or(StatusError::Unavailable)?;
        self.decrypt(id, &stored)
    }

    async fn get_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<String, StatusError> {
        let stored: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT ciphertext FROM status_webhook_secrets WHERE subscription_id=$1",
        )
        .bind(id)
        .fetch_optional(&mut **tx)
        .await?;
        self.decrypt(id, &stored.ok_or(StatusError::Unavailable)?)
    }

    async fn delete_in_transaction(
        &self,
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        id: Uuid,
    ) -> Result<(), StatusError> {
        sqlx::query("DELETE FROM status_webhook_secrets WHERE subscription_id=$1")
            .bind(id)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
