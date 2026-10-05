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
    nbf: Option<u64>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_metadata: Option<SupabaseUserMetadata>,
    #[serde(default)]
    aud: Option<serde_json::Value>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    is_anonymous: Option<bool>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    amr: Vec<serde_json::Value>,
    #[serde(default)]
    vox_identity: Option<IdentityPin>,
}

#[derive(Debug, Deserialize)]
struct IdentityPin {
    version: u32,
    session_id: String,
    user_id: String,
    identity_id: String,
    provider: String,
}

#[derive(Debug, Deserialize)]
struct SupabaseUser {
    id: String,
    #[serde(default)]
    is_anonymous: Option<bool>,
    #[serde(default)]
    identities: Vec<SupabaseIdentity>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    user_metadata: Option<SupabaseUserMetadata>,
}

#[derive(Debug, Deserialize)]
struct SupabaseIdentity {
    identity_id: String,
    user_id: String,
    provider: String,
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
    base_url: String,
    keys: Vec<serde_json::Value>,
}

fn jwks_cache() -> &'static Mutex<Option<CachedJwks>> {
    static CACHE: OnceLock<Mutex<Option<CachedJwks>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(None))
}

fn verify_hs256_claims(
    token: &str,
    secret: Option<&str>,
    expected_issuer: &str,
) -> Result<TokenClaims, StatusCode> {
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
    let claims: TokenClaims =
        serde_json::from_slice(&decode_part(payload)?).map_err(|_| StatusCode::UNAUTHORIZED)?;
    validate_supabase_claims(&claims, expected_issuer)?;
    Ok(claims)
}

pub async fn verify_id_token(token: &str) -> Result<VerifiedIdentity, StatusCode> {
    let token = token.trim();
    if token.is_empty() || token.len() > 32 * 1024 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let (header, _, _) = split_jwt(token)?;
    let header_json = decode_part(header)?;
    let header_value: serde_json::Value =
        serde_json::from_slice(&header_json).map_err(|_| StatusCode::UNAUTHORIZED)?;
    match header_value.get("alg").and_then(|value| value.as_str()) {
        Some("HS256") => {
            let base = supabase_base_url().ok_or(StatusCode::UNAUTHORIZED)?;
            let secret = std::env::var("SUPABASE_JWT_SECRET").ok();
            let claims = verify_hs256_claims(token, secret.as_deref(), &format!("{base}/auth/v1"))?;
            verify_current_supabase_identity(token, &base, claims).await
        }
        Some("ES256") => verify_supabase_asymmetric(token, Algorithm::ES256).await,
        Some("RS256") => {
            // Unverified issuer is used only to choose a verifier, never as authority.
            let (_, payload, _) = split_jwt(token)?;
            let payload: serde_json::Value = serde_json::from_slice(&decode_part(payload)?)
                .map_err(|_| StatusCode::UNAUTHORIZED)?;
            if let Some(base) = supabase_base_url()
                && payload.get("iss").and_then(serde_json::Value::as_str)
                    == Some(format!("{base}/auth/v1").as_str())
            {
                verify_supabase_asymmetric(token, Algorithm::RS256).await
            } else {
                verify_google_token(token).await
            }
        }
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

fn validate_supabase_claims(claims: &TokenClaims, issuer: &str) -> Result<(), StatusCode> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if claims.iss.as_deref() != Some(issuer)
        || claims.aud.as_ref().and_then(serde_json::Value::as_str) != Some("authenticated")
        || claims.role.as_deref() != Some("authenticated")
        || claims.is_anonymous != Some(false)
        || claims.exp.is_none_or(|exp| exp <= now)
        || claims.nbf.is_some_and(|nbf| nbf > now)
        || uuid::Uuid::parse_str(&claims.sub).is_err()
        || claims
            .session_id
            .as_deref()
            .is_none_or(|id| uuid::Uuid::parse_str(id).is_err())
        || claims.vox_identity.is_none()
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    Ok(())
}

fn bind_supabase_identity(
    claims: &TokenClaims,
    user: SupabaseUser,
) -> Result<VerifiedIdentity, StatusCode> {
    let pin = claims
        .vox_identity
        .as_ref()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if user.id != claims.sub || user.is_anonymous != Some(false) || user.identities.len() != 1 {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let identity = &user.identities[0];
    if pin.version != 1
        || Some(pin.session_id.as_str()) != claims.session_id.as_deref()
        || pin.user_id != user.id
        || identity.user_id != user.id
        || pin.identity_id != identity.identity_id
        || pin.provider != identity.provider
        || identity.identity_id.len() != 36
        || uuid::Uuid::parse_str(&identity.identity_id).is_err()
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let allowed: &[&str] = match identity.provider.as_str() {
        "google" => &["oauth"],
        "email" => &["otp", "magiclink", "email/signup"],
        _ => return Err(StatusCode::UNAUTHORIZED),
    };
    let methods: Vec<Option<&str>> = claims
        .amr
        .iter()
        .map(|entry| entry.get("method").and_then(serde_json::Value::as_str))
        .collect();
    if !methods
        .iter()
        .any(|method| method.is_some_and(|method| allowed.contains(&method)))
        || methods.iter().any(|method| {
            !method.is_some_and(|method| {
                allowed.contains(&method) || matches!(method, "token_refresh" | "totp")
            })
        })
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let name = user
        .user_metadata
        .as_ref()
        .and_then(|metadata| metadata.full_name.as_deref().or(metadata.name.as_deref()))
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);
    Ok(VerifiedIdentity {
        issuer: claims.iss.clone().ok_or(StatusCode::UNAUTHORIZED)?,
        subject: format!("supabase:{}:{}", identity.provider, identity.identity_id),
        email: user.email.filter(|email| !email.trim().is_empty()),
        name,
    })
}

async fn verify_current_supabase_identity(
    token: &str,
    base: &str,
    claims: TokenClaims,
) -> Result<VerifiedIdentity, StatusCode> {
    let key = std::env::var("SUPABASE_PUBLISHABLE_KEY")
        .or_else(|_| std::env::var("SUPABASE_ANON_KEY"))
        .ok()
        .filter(|key| !key.trim().is_empty())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let user = fetch_supabase_user(token, base, &key).await?;
    bind_supabase_identity(&claims, user)
}

async fn fetch_supabase_user(
    token: &str,
    base: &str,
    key: &str,
) -> Result<SupabaseUser, StatusCode> {
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(5))
        .build()
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let response = client
        .get(format!("{base}/auth/v1/user"))
        .header("apikey", key)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !response.status().is_success() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let bytes = bounded_body(response).await?;
    serde_json::from_slice(&bytes).map_err(|_| StatusCode::UNAUTHORIZED)
}

async fn bounded_body(mut response: reqwest::Response) -> Result<Vec<u8>, StatusCode> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?
    {
        if bytes.len() + chunk.len() > 1024 * 1024 {
            return Err(StatusCode::UNAUTHORIZED);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
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
        email: claims
            .email
            .clone()
            .filter(|value| !value.trim().is_empty()),
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
    std::env::var("SUPABASE_URL")
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_string())
        .filter(|value| !value.is_empty())
}

fn verify_asymmetric_components(
    token: &str,
    alg: Algorithm,
    x: &str,
    y: &str,
    n: Option<&str>,
    e: Option<&str>,
    expected_issuer: &str,
) -> Result<TokenClaims, StatusCode> {
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
    validation.set_audience(&["authenticated"]);
    validation.leeway = 0;
    validation.validate_nbf = true;
    validation.set_issuer(&[expected_issuer]);
    let data =
        decode::<TokenClaims>(token, &key, &validation).map_err(|_| StatusCode::UNAUTHORIZED)?;
    validate_supabase_claims(&data.claims, expected_issuer)?;
    Ok(data.claims)
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
    let claims = match alg {
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
    }?;
    verify_current_supabase_identity(token, &base, claims).await
}

async fn supabase_jwk(base_url: &str, kid: &str) -> Result<serde_json::Value, StatusCode> {
    if let Some(cached) = jwks_cache().lock().ok().and_then(|guard| guard.clone())
        && cached.base_url == base_url
        && cached.fetched_at.elapsed() < Duration::from_secs(300)
        && let Some(key) = cached
            .keys
            .iter()
            .find(|key| key.get("kid").and_then(|value| value.as_str()) == Some(kid))
    {
        return Ok(key.clone());
    }

    let url = format!("{base_url}/auth/v1/.well-known/jwks.json");
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| StatusCode::UNAUTHORIZED)?
        .get(&url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    if !response.status().is_success() {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let body: serde_json::Value = serde_json::from_slice(&bounded_body(response).await?)
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    let keys = body
        .get("keys")
        .and_then(|value| value.as_array())
        .cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if let Ok(mut guard) = jwks_cache().lock() {
        *guard = Some(CachedJwks {
            fetched_at: Instant::now(),
            base_url: base_url.to_owned(),
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
mod tests {
    use super::*;
    use serde_json::{Value, json};
    const ISSUER: &str = "https://fixture.supabase.co/auth/v1";
    const USER: &str = "7b7e964f-339f-4b68-a17b-cdf114d336b8";
    const SESSION: &str = "d564018b-e2cd-4624-bd80-19f6f6c20f48";
    const IDENTITY: &str = "f5b2d9b8-6b06-4259-8b9a-7d82d59f93cb";
    const SECRET: &str = "fixture-signing-secret-not-a-provider-secret";
    fn claims(provider: &str) -> Value {
        json!({"sub":USER,"iss":ISSUER,"exp":chrono::Utc::now().timestamp()+300,
            "aud":"authenticated","role":"authenticated","is_anonymous":false,
            "session_id":SESSION,"amr":[{"method":if provider=="google" {"oauth"} else {"otp"}}],
            "vox_identity":{"version":1,"session_id":SESSION,"user_id":USER,"identity_id":IDENTITY,"provider":provider}})
    }
    fn user(provider: &str) -> Value {
        json!({"id":USER,"is_anonymous":false,"email":"display@example.test",
            "user_metadata":{"full_name":"Fresh display name","vox_identity":{"identity_id":"forged-user-metadata"}},
            "identities":[{"identity_id":IDENTITY,"user_id":USER,"provider":provider}]})
    }
    fn sign(claims: &Value, secret: &str) -> String {
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            claims,
            &jsonwebtoken::EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap()
    }
    fn verified(claims: &Value, user: Value) -> Result<VerifiedIdentity, StatusCode> {
        let claims = verify_hs256_claims(&sign(claims, SECRET), Some(SECRET), ISSUER)?;
        bind_supabase_identity(&claims, serde_json::from_value(user).unwrap())
    }
    #[test]
    fn signed_identity_pin_requires_current_sole_identity_and_method() {
        for provider in ["google", "email"] {
            let result = verified(&claims(provider), user(provider)).unwrap();
            assert_eq!(result.subject, format!("supabase:{provider}:{IDENTITY}"));
            assert_eq!(result.issuer, ISSUER);
            assert_eq!(result.name.as_deref(), Some("Fresh display name"));
            let mut refreshed = claims(provider);
            refreshed["amr"]
                .as_array_mut()
                .unwrap()
                .push(json!({"method":"token_refresh"}));
            assert_eq!(
                verified(&refreshed, user(provider)).unwrap().subject,
                result.subject
            );
        }
        let mut replaced = user("google");
        replaced["identities"][0]["identity_id"] = json!("b89662f1-10d0-49b5-8b60-9b60d9f44c37");
        assert_eq!(
            verified(&claims("google"), replaced),
            Err(StatusCode::UNAUTHORIZED)
        );
        let mut fresh_replacement = user("google");
        let replacement_id = "b89662f1-10d0-49b5-8b60-9b60d9f44c37";
        fresh_replacement["identities"][0]["identity_id"] = json!(replacement_id);
        let mut new_pin = claims("google");
        new_pin["vox_identity"]["identity_id"] = json!(replacement_id);
        assert_eq!(
            verified(&new_pin, fresh_replacement).unwrap().subject,
            format!("supabase:google:{replacement_id}")
        );
        assert_ne!(
            verified(&new_pin, user("google"))
                .ok()
                .map(|identity| identity.subject),
            Some(format!("supabase:google:{IDENTITY}"))
        );
        let mut merged = user("google");
        merged["identities"]
            .as_array_mut()
            .unwrap()
            .push(user("email")["identities"][0].clone());
        assert_eq!(
            verified(&claims("google"), merged),
            Err(StatusCode::UNAUTHORIZED)
        );
        for (field, value) in [("id", json!(SESSION)), ("is_anonymous", json!(true))] {
            let mut changed = user("google");
            changed[field] = value;
            assert_eq!(
                verified(&claims("google"), changed),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        for (field, value) in [
            ("session_id", json!(IDENTITY)),
            ("user_id", json!(SESSION)),
            ("identity_id", json!(SESSION)),
            ("provider", json!("email")),
            ("version", json!(2)),
        ] {
            let mut changed = claims("google");
            changed["vox_identity"][field] = value;
            assert_eq!(
                verified(&changed, user("google")),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
        for methods in [
            json!([]),
            json!([{"method":"password"}]),
            json!([{"method":"oauth"},{"method":"password"}]),
            json!([{"method":"token_refresh"}]),
            json!([{"method":"otp"}]),
            json!([{}]),
        ] {
            let mut changed = claims("google");
            changed["amr"] = methods;
            assert_eq!(
                verified(&changed, user("google")),
                Err(StatusCode::UNAUTHORIZED)
            );
        }
    }
    #[test]
    fn forged_unsigned_or_wrong_authority_tokens_fail_closed() {
        let good = claims("google");
        let forged = sign(&good, "attacker-signing-key");
        assert!(verify_hs256_claims(&forged, Some(SECRET), ISSUER).is_err());
        let unsigned = format!(
            "{}.{}.unsigned",
            URL_SAFE_NO_PAD.encode(br#"{"alg":"none"}"#),
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&good).unwrap())
        );
        assert!(verify_hs256_claims(&unsigned, Some(SECRET), ISSUER).is_err());
        let signed = sign(&good, SECRET);
        let (header, _, signature) = split_jwt(&signed).unwrap();
        let mut forged_pin = good.clone();
        forged_pin["vox_identity"]["identity_id"] = json!(SESSION);
        let tampered = format!(
            "{header}.{}.{signature}",
            URL_SAFE_NO_PAD.encode(serde_json::to_vec(&forged_pin).unwrap())
        );
        assert!(verify_hs256_claims(&tampered, Some(SECRET), ISSUER).is_err());
        for (field, value) in [
            ("iss", json!("https://other.supabase.co/auth/v1")),
            ("aud", json!("service_role")),
            ("aud", json!(["authenticated"])),
            ("role", json!("service_role")),
            ("is_anonymous", json!(true)),
            ("exp", json!(1)),
            ("nbf", json!(chrono::Utc::now().timestamp() + 300)),
            ("session_id", json!("invalid")),
            ("vox_identity", Value::Null),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(
                verify_hs256_claims(&sign(&bad, SECRET), Some(SECRET), ISSUER).is_err(),
                "accepted {field}"
            );
        }
        let mut without_pin = good.clone();
        without_pin.as_object_mut().unwrap().remove("vox_identity");
        without_pin["user_metadata"] = json!({"vox_identity":good["vox_identity"]});
        assert!(verify_hs256_claims(&sign(&without_pin, SECRET), Some(SECRET), ISSUER).is_err());
    }
    #[test]
    fn asymmetric_tokens_enforce_same_issuer_audience_and_signed_pin() {
        use ring::signature::KeyPair;
        let rng = ring::rand::SystemRandom::new();
        let algorithm = &ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING;
        let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(algorithm, &rng).unwrap();
        let key =
            ring::signature::EcdsaKeyPair::from_pkcs8(algorithm, pkcs8.as_ref(), &rng).unwrap();
        let point = key.public_key().as_ref();
        let x = URL_SAFE_NO_PAD.encode(&point[1..33]);
        let y = URL_SAFE_NO_PAD.encode(&point[33..65]);
        let signed = |claims: &Value| {
            let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"ES256"}"#);
            let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(claims).unwrap());
            let message = format!("{header}.{payload}");
            let signature = key.sign(&rng, message.as_bytes()).unwrap();
            format!("{message}.{}", URL_SAFE_NO_PAD.encode(signature.as_ref()))
        };
        let good = claims("google");
        let verified_claims = verify_asymmetric_components(
            &signed(&good),
            Algorithm::ES256,
            &x,
            &y,
            None,
            None,
            ISSUER,
        )
        .unwrap();
        assert!(
            bind_supabase_identity(
                &verified_claims,
                serde_json::from_value(user("google")).unwrap()
            )
            .is_ok()
        );
        for (field, value) in [
            ("iss", json!("https://other.supabase.co/auth/v1")),
            ("aud", json!("other")),
            ("role", json!("service_role")),
            ("vox_identity", Value::Null),
        ] {
            let mut bad = good.clone();
            bad[field] = value;
            assert!(
                verify_asymmetric_components(
                    &signed(&bad),
                    Algorithm::ES256,
                    &x,
                    &y,
                    None,
                    None,
                    ISSUER
                )
                .is_err()
            );
        }
    }
    #[tokio::test]
    async fn fresh_user_request_binds_identity_and_does_not_follow_redirects() {
        use axum::{Json, Router, http::HeaderMap, routing::get};
        let app = Router::new()
            .route(
                "/auth/v1/user",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers["apikey"], "fixture-public-key");
                    assert_eq!(headers["authorization"], "Bearer fixture-token");
                    Json(user("google"))
                }),
            )
            .route(
                "/redirect/auth/v1/user",
                get(|| async { axum::response::Redirect::temporary("/auth/v1/user") }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let current = fetch_supabase_user("fixture-token", &base, "fixture-public-key")
            .await
            .unwrap();
        assert!(
            bind_supabase_identity(
                &verify_hs256_claims(&sign(&claims("google"), SECRET), Some(SECRET), ISSUER)
                    .unwrap(),
                current
            )
            .is_ok()
        );
        assert!(
            fetch_supabase_user(
                "fixture-token",
                &format!("{base}/redirect"),
                "fixture-public-key"
            )
            .await
            .is_err()
        );
        server.abort();
    }
}
