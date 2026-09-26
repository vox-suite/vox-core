use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::{
    aead::{self, Aad, LessSafeKey, Nonce, UnboundKey},
    rand::{SecureRandom, SystemRandom},
};
use sha2::{Digest, Sha256};

use super::ConnectedAppError;

/// AES-256-GCM for OAuth secrets at rest. Every value is bound to the row it
/// belongs to through the associated data, so a ciphertext copied into another
/// row does not decrypt.
#[derive(Clone)]
pub struct CredentialCipher {
    key: [u8; 32],
}

impl CredentialCipher {
    pub fn from_hex_key(value: &str) -> Result<Self, ConnectedAppError> {
        let bytes = hex::decode(value.trim()).map_err(|_| ConnectedAppError::NotConfigured)?;
        let key: [u8; 32] = bytes
            .try_into()
            .map_err(|_| ConnectedAppError::NotConfigured)?;
        Ok(Self { key })
    }

    fn cipher(&self) -> Result<LessSafeKey, ConnectedAppError> {
        let key = UnboundKey::new(&aead::AES_256_GCM, &self.key)
            .map_err(|_| ConnectedAppError::NotConfigured)?;
        Ok(LessSafeKey::new(key))
    }

    pub fn seal(&self, aad: &[u8], plaintext: &str) -> Result<Vec<u8>, ConnectedAppError> {
        let mut nonce = [0u8; 12];
        SystemRandom::new()
            .fill(&mut nonce)
            .map_err(|_| ConnectedAppError::Crypto)?;
        let mut body = plaintext.as_bytes().to_vec();
        self.cipher()?
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| ConnectedAppError::Crypto)?;
        let mut stored = Vec::with_capacity(12 + body.len());
        stored.extend_from_slice(&nonce);
        stored.extend_from_slice(&body);
        Ok(stored)
    }

    pub fn open(&self, aad: &[u8], stored: &[u8]) -> Result<String, ConnectedAppError> {
        let (nonce, body) = stored
            .split_at_checked(12)
            .ok_or(ConnectedAppError::Crypto)?;
        let nonce: [u8; 12] = nonce.try_into().map_err(|_| ConnectedAppError::Crypto)?;
        let mut body = body.to_vec();
        let opened = self
            .cipher()?
            .open_in_place(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut body,
            )
            .map_err(|_| ConnectedAppError::Crypto)?;
        String::from_utf8(opened.to_vec()).map_err(|_| ConnectedAppError::Crypto)
    }
}

/// URL-safe random token with 256 bits of entropy (OAuth state, PKCE verifier).
pub fn random_token() -> Result<String, ConnectedAppError> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| ConnectedAppError::Crypto)?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

/// RFC 7636 S256 code challenge.
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub fn sha256_hex(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}
