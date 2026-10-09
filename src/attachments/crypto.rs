use base64::{Engine as _, engine::general_purpose::STANDARD};
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    #[error("cryptographic operation failed")]
    OperationFailed,
    #[error("invalid key format")]
    InvalidKey,
    #[error("decryption failed or authentication tag mismatch")]
    DecryptionFailed,
}

pub fn derive_key(seed: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(seed.as_bytes());
    let result = hasher.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&result[..32]);
    key
}

pub fn attachment_master_key() -> Result<[u8; 32], CryptoError> {
    let seed = std::env::var("VOX_ATTACHMENT_SECRET_KEY").or_else(|_| std::env::var("VOX_CREDENTIAL_KEY"))
        .map_err(|_| CryptoError::InvalidKey)?;
    if seed.len() < 32 { return Err(CryptoError::InvalidKey); }
    Ok(derive_key(&format!("vox-attachment-v1:{seed}")))
}

pub fn encrypt_attachment_secret(
    key: &[u8; 32],
    job_id: Uuid,
    attachment_id: Uuid,
    plaintext: &str,
) -> Result<(String, String), CryptoError> {
    let unbound =
        UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| CryptoError::OperationFailed)?;
    let safe_key = LessSafeKey::new(unbound);

    let mut nonce_bytes = [0u8; 12];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| CryptoError::OperationFailed)?;

    let nonce = Nonce::assume_unique_for_key(nonce_bytes);
    let aad_tag = format!("{}:{}", job_id, attachment_id);
    let aad = Aad::from(aad_tag.as_bytes());

    let mut in_out = plaintext.as_bytes().to_vec();
    safe_key
        .seal_in_place_append_tag(nonce, aad, &mut in_out)
        .map_err(|_| CryptoError::OperationFailed)?;

    let ciphertext_b64 = STANDARD.encode(&in_out);
    let nonce_b64 = STANDARD.encode(nonce_bytes);

    Ok((ciphertext_b64, nonce_b64))
}

pub fn decrypt_attachment_secret(
    key: &[u8; 32],
    job_id: Uuid,
    attachment_id: Uuid,
    ciphertext_b64: &str,
    nonce_b64: &str,
) -> Result<String, CryptoError> {
    let unbound =
        UnboundKey::new(&aead::AES_256_GCM, key).map_err(|_| CryptoError::OperationFailed)?;
    let safe_key = LessSafeKey::new(unbound);

    let nonce_bytes = STANDARD
        .decode(nonce_b64)
        .map_err(|_| CryptoError::DecryptionFailed)?;
    if nonce_bytes.len() != 12 {
        return Err(CryptoError::DecryptionFailed);
    }
    let mut nonce_arr = [0u8; 12];
    nonce_arr.copy_from_slice(&nonce_bytes);
    let nonce = Nonce::assume_unique_for_key(nonce_arr);

    let aad_tag = format!("{}:{}", job_id, attachment_id);
    let aad = Aad::from(aad_tag.as_bytes());

    let mut in_out = STANDARD
        .decode(ciphertext_b64)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    let opened = safe_key
        .open_in_place(nonce, aad, &mut in_out)
        .map_err(|_| CryptoError::DecryptionFailed)?;

    String::from_utf8(opened.to_vec()).map_err(|_| CryptoError::DecryptionFailed)
}
