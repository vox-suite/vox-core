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
