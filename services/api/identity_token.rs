/**
* Verification of provider ID tokens used to mint Vox sessions.
*/
use axum::http::StatusCode;
use base64::Engine;
use base64::engine::general_purpose::{URL_SAFE, URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use serde::Deserialize;
use sha2::Sha256;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub name: Option<String>,
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
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_metadata: Option<SupabaseUserMetadata>,
}

#[derive(Debug, Deserialize)]
struct SupabaseUserMetadata {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn claims_name(claims: &TokenClaims) -> Option<String> {
    let direct = claims
        .name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    direct.or_else(|| {
        claims.user_metadata.as_ref().and_then(|metadata| {
            metadata
                .full_name
                .as_deref()
                .or(metadata.name.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
        })
    })
}

#[derive(Clone)]
struct CachedJwks {
    fetched_at: Instant,
    keys: Vec<serde_json::Value>,
}

fn jwks_cache() -> &'static Mutex<Option<CachedJwks>> {
    static CACHE: OnceLock<Mutex<Option<CachedJwks>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
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
    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| StatusCode::UNAUTHORIZED)?;
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
        // New Supabase projects sign access tokens with asymmetric JWT signing keys (ES256).
        Some("ES256") => verify_supabase_asymmetric(token, Algorithm::ES256).await,
        Some("RS256") => {
            // Only a configured Supabase project is a trusted issuer.
            if supabase_base_url().is_some() {
                match verify_supabase_asymmetric(token, Algorithm::RS256).await {
                    Ok(identity) => Ok(identity),
                    Err(_) => verify_google_token(token).await,
                }
            } else {
                verify_google_token(token).await
            }
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn identity_from_payload(
    payload: &[u8],
    fallback_issuer: &str,
) -> Result<VerifiedIdentity, StatusCode> {
    let claims: TokenClaims =
        serde_json::from_slice(payload).map_err(|_| StatusCode::UNAUTHORIZED)?;
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
    let name = claims_name(&claims);
    Ok(VerifiedIdentity {
        issuer,
        subject: subject.to_string(),
        email: claims.email.filter(|value| !value.trim().is_empty()),
        name,
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

fn supabase_base_url() -> Option<String> {
    ["SUPABASE_URL", "VOX_SUPABASE_URL"]
        .iter()
        .find_map(|key| std::env::var(key).ok())
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

#[allow(dead_code)]
pub fn verify_es256_components(
    token: &str,
    x: &str,
    y: &str,
    expected_issuer: &str,
) -> Result<VerifiedIdentity, StatusCode> {
    verify_asymmetric_components(token, Algorithm::ES256, x, y, None, None, expected_issuer)
}

fn verify_asymmetric_components(
    token: &str,
    alg: Algorithm,
    x: &str,
    y: &str,
    n: Option<&str>,
    e: Option<&str>,
    expected_issuer: &str,
) -> Result<VerifiedIdentity, StatusCode> {
    let key = match alg {
        Algorithm::ES256 => {
            DecodingKey::from_ec_components(x, y).map_err(|_| StatusCode::UNAUTHORIZED)?
        }
        Algorithm::RS256 => {
            let (n, e) = (
                n.ok_or(StatusCode::UNAUTHORIZED)?,
                e.ok_or(StatusCode::UNAUTHORIZED)?,
            );
            DecodingKey::from_rsa_components(n, e).map_err(|_| StatusCode::UNAUTHORIZED)?
        }
        _ => return Err(StatusCode::UNAUTHORIZED),
    };
    let mut validation = Validation::new(alg);
    validation.validate_aud = false;
    validation.set_issuer(&[expected_issuer]);
    let data =
        decode::<TokenClaims>(token, &key, &validation).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let subject = data.claims.sub.trim();
    if subject.is_empty() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(VerifiedIdentity {
        issuer: data
            .claims
            .iss
            .clone()
            .unwrap_or_else(|| expected_issuer.to_string()),
        subject: subject.to_string(),
        email: data
            .claims
            .email
            .clone()
            .filter(|value| !value.trim().is_empty()),
        name: claims_name(&data.claims),
    })
}

async fn verify_supabase_asymmetric(
    token: &str,
    alg: Algorithm,
) -> Result<VerifiedIdentity, StatusCode> {
    let base = supabase_base_url().ok_or(StatusCode::UNAUTHORIZED)?;
    let expected_issuer = format!("{base}/auth/v1");
    let (header, _, _) = split_jwt(token)?;
    let header_json: serde_json::Value =
        serde_json::from_slice(&decode_part(header)?).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let kid = header_json
        .get("kid")
        .and_then(|value| value.as_str())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let jwk = supabase_jwk(&base, kid).await?;
    match alg {
        Algorithm::ES256 => {
            let x = jwk
                .get("x")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;
            let y = jwk
                .get("y")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;
            verify_asymmetric_components(token, alg, x, y, None, None, &expected_issuer)
        }
        Algorithm::RS256 => {
            let n = jwk
                .get("n")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;
            let e = jwk
                .get("e")
                .and_then(|v| v.as_str())
                .ok_or(StatusCode::UNAUTHORIZED)?;
            verify_asymmetric_components(token, alg, "", "", Some(n), Some(e), &expected_issuer)
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

async fn supabase_jwk(base_url: &str, kid: &str) -> Result<serde_json::Value, StatusCode> {
    if let Some(cached) = jwks_cache().lock().ok().and_then(|guard| guard.clone())
        && cached.fetched_at.elapsed() < Duration::from_secs(300)
        && let Some(key) = cached
            .keys
            .iter()
            .find(|key| key.get("kid").and_then(|value| value.as_str()) == Some(kid))
    {
        return Ok(key.clone());
    }

    let url = format!("{base_url}/auth/v1/.well-known/jwks.json");
    let response = reqwest::Client::new()
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !response.status().is_success() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let keys = body
        .get("keys")
        .and_then(|value| value.as_array())
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if let Ok(mut guard) = jwks_cache().lock() {
        *guard = Some(CachedJwks {
            fetched_at: Instant::now(),
            keys: keys.clone(),
        });
    }
    keys.into_iter()
        .find(|key| key.get("kid").and_then(|value| value.as_str()) == Some(kid))
        .ok_or(StatusCode::UNAUTHORIZED)
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
    if identity.issuer != "https://accounts.google.com" && identity.issuer != "accounts.google.com"
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let payload_json: serde_json::Value =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let token_audience = payload_json
        .get("aud")
        .and_then(|value| value.as_str())
        .unwrap_or("");
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
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let keys = body
        .get("keys")
        .and_then(|value| value.as_array())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let key = keys
        .iter()
        .find(|key| key.get("kid").and_then(|value| value.as_str()) == Some(kid))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    Ok(GoogleKey {
        n: key
            .get("n")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        e: key
            .get("e")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
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
    let signature = Signature::try_from(decode_part(signature)?.as_slice())
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok(verifier.verify(message.as_bytes(), &signature).is_ok())
}

#[cfg(test)]
#[path = "../../tests/unit/identity_token.rs"]
mod tests;
