/**
* Verification of provider ID tokens used to mint Vox sessions.
*/
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use sha2::Sha256;
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TokenClaims {
    sub: String,
    #[serde(default)]
    iss: Option<String>,
    #[serde(default)]
    exp: Option<u64>,
    #[serde(default)]
    email: Option<String>,
}

pub fn verify_hs256_jwt(token: &str, secret: Option<&str>) -> Result<VerifiedIdentity, StatusCode> {
    let secret = secret
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let (header, payload, signature) = split_jwt(token)?;
    let header_json = decode_part(header)?;
    let header_value: serde_json::Value =
        serde_json::from_slice(&header_json).map_err(|_| StatusCode::UNAUTHORIZED)?;
    if header_value.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    mac.update(format!("{header}.{payload}").as_bytes());
    let expected = mac.finalize().into_bytes();
    let actual = decode_part(signature)?;
    if expected.len() != actual.len()
        || expected.as_slice().ct_eq(actual.as_slice()).unwrap_u8() != 1
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    identity_from_payload(&decode_part(payload)?, "supabase")
}

pub async fn verify_id_token(token: &str) -> Result<VerifiedIdentity, StatusCode> {
    let token = token.trim();
    if token.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (header, _, _) = split_jwt(token)?;
    let header_json = decode_part(header)?;
    let header_value: serde_json::Value =
        serde_json::from_slice(&header_json).map_err(|_| StatusCode::UNAUTHORIZED)?;
    match header_value.get("alg").and_then(|value| value.as_str()) {
        Some("HS256") => {
            let secret = std::env::var("SUPABASE_JWT_SECRET").ok();
            verify_hs256_jwt(token, secret.as_deref())
        }
        Some("RS256") => verify_google_token(token).await,
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn identity_from_payload(payload: &[u8], fallback_issuer: &str) -> Result<VerifiedIdentity, StatusCode> {
    let claims: TokenClaims = serde_json::from_slice(payload).map_err(|_| StatusCode::UNAUTHORIZED)?;
    if let Some(exp) = claims.exp {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if exp <= now {
            return Err(StatusCode::UNAUTHORIZED);
        }
    } else {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let subject = claims.sub.trim();
    if subject.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let issuer = claims
        .iss
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(fallback_issuer)
        .to_string();
    Ok(VerifiedIdentity {
        issuer,
        subject: subject.to_string(),
        email: claims.email.filter(|value| !value.trim().is_empty()),
    })
}

fn split_jwt(token: &str) -> Result<(&str, &str, &str), StatusCode> {
    let mut parts = token.split('.');
    let header = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    let payload = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    let signature = parts.next().ok_or(StatusCode::UNAUTHORIZED)?;
    if parts.next().is_some() || header.is_empty() || payload.is_empty() || signature.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok((header, payload, signature))
}

fn decode_part(value: &str) -> Result<Vec<u8>, StatusCode> {
    URL_SAFE_NO_PAD
        .decode(value)
        .or_else(|_| URL_SAFE.decode(value))
        .map_err(|_| StatusCode::UNAUTHORIZED)
}

async fn verify_google_token(token: &str) -> Result<VerifiedIdentity, StatusCode> {
    let audience = std::env::var("GOOGLE_OAUTH_CLIENT_ID")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let (header, payload, signature) = split_jwt(token)?;
    let header_json: serde_json::Value =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let kid = header_json
        .get("kid")
        .and_then(|value| value.as_str())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let jwk = google_jwk(kid).await?;
    if !rsa_sha256_valid(&jwk, &format!("{header}.{payload}"), signature)? {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let identity = identity_from_payload(&decode_part(payload)?, "https://accounts.google.com")?;
    if identity.issuer != "https://accounts.google.com" && identity.issuer != "accounts.google.com" {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let payload_json: serde_json::Value =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let token_audience = payload_json.get("aud").and_then(|value| value.as_str()).unwrap_or("");
    if token_audience != audience.trim() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(identity)
}

struct GoogleKey {
    n: String,
    e: String,
}

async fn google_jwk(kid: &str) -> Result<GoogleKey, StatusCode> {
    let response = reqwest::Client::new()
        .get("https://www.googleapis.com/oauth2/v3/certs")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !response.status().is_success() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body: serde_json::Value = response.json().await.map_err(|_| StatusCode::UNAUTHORIZED)?;
    let keys = body.get("keys").and_then(|value| value.as_array()).ok_or(StatusCode::UNAUTHORIZED)?;
    let key = keys
        .iter()
        .find(|key| key.get("kid").and_then(|value| value.as_str()) == Some(kid))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    Ok(GoogleKey {
        n: key.get("n").and_then(|value| value.as_str()).unwrap_or("").to_string(),
        e: key.get("e").and_then(|value| value.as_str()).unwrap_or("").to_string(),
    })
}

fn rsa_sha256_valid(key: &GoogleKey, message: &str, signature: &str) -> Result<bool, StatusCode> {
    use rsa::pkcs1v15::{Signature, VerifyingKey};
    use rsa::signature::Verifier;
    use rsa::{BigUint, RsaPublicKey};

    let modulus = BigUint::from_bytes_be(&decode_part(&key.n)?);
    let exponent = BigUint::from_bytes_be(&decode_part(&key.e)?);
    let public_key = RsaPublicKey::new(modulus, exponent).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let verifier = VerifyingKey::<rsa::sha2::Sha256>::new(public_key);
    let signature =
        Signature::try_from(decode_part(signature)?.as_slice()).map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok(verifier.verify(message.as_bytes(), &signature).is_ok())
}

#[cfg(test)]
mod tests {
    use super::verify_hs256_jwt;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    fn b64(bytes: &[u8]) -> String {
        URL_SAFE_NO_PAD.encode(bytes)
    }

    fn token(secret: &str, subject: &str) -> String {
        let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = b64(
            format!(
                r#"{{"sub":"{subject}","iss":"https://example.supabase.co/auth/v1","exp":4000000000}}"#
            )
            .as_bytes(),
        );
        let signing_input = format!("{header}.{payload}");
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("mac");
        mac.update(signing_input.as_bytes());
        let signature = b64(&mac.finalize().into_bytes());
        format!("{signing_input}.{signature}")
    }

    #[test]
    fn missing_secret_rejects_jwt() {
        let bearer = token("secret", "user-1");
        assert!(verify_hs256_jwt(&bearer, None).is_err());
        assert!(verify_hs256_jwt(&bearer, Some("  ")).is_err());
    }

    #[test]
    fn wrong_secret_rejects_jwt() {
        let bearer = token("secret", "user-1");
        assert!(verify_hs256_jwt(&bearer, Some("other")).is_err());
    }

    #[test]
    fn valid_signature_returns_issuer_and_subject() {
        let bearer = token("secret", "user-1");
        let identity = verify_hs256_jwt(&bearer, Some("secret")).expect("verified");
        assert_eq!(identity.subject, "user-1");
        assert_eq!(identity.issuer, "https://example.supabase.co/auth/v1");
    }
}
