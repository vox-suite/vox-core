use super::{verify_es256_components, verify_hs256_jwt};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, Mac};
use sha2::Sha256;

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn token(secret: &str, subject: &str) -> String {
    let header = b64(br#"{"alg":"HS256","typ":"JWT"}"#);
    let payload = b64(format!(
        r#"{{"sub":"{subject}","iss":"https://example.supabase.co/auth/v1","exp":4000000000}}"#
    )
    .as_bytes());
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

#[test]
fn valid_es256_signature_returns_identity() {
    // Generated with OpenSSL P-256 + Node crypto.sign ieee-p1363.
    let x = "MSRJYeAnerL26Ek-X-0ipKbgdKTbWvEeOByXO2QL6cE";
    let y = "9dloNwMe1N7IYsQUBzxwhBW0MMUR7hzGuZ4Qhkh3Wmg";
    let bearer = "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InRlc3Qta2lkIn0.eyJzdWIiOiIxMTExMTExMS0xMTExLTExMTEtMTExMS0xMTExMTExMTExMTEiLCJpc3MiOiJodHRwczovL2V4YW1wbGUuc3VwYWJhc2UuY28vYXV0aC92MSIsImV4cCI6NDAwMDAwMDAwMCwiZW1haWwiOiJhQGIuY28ifQ.1-jlvzKyXTavpVgffnxpKd5oKcxdli52HK2_qDGUGUAvHd14Ryi5Nc7o0FpF216yx-1t4zLucjv4sawuWRBmdA";
    let identity = verify_es256_components(bearer, x, y, "https://example.supabase.co/auth/v1")
        .expect("verified");
    assert_eq!(identity.subject, "11111111-1111-1111-1111-111111111111");
    assert_eq!(identity.issuer, "https://example.supabase.co/auth/v1");
    assert_eq!(identity.email.as_deref(), Some("a@b.co"));
}

#[test]
fn es256_rejects_wrong_issuer() {
    let x = "MSRJYeAnerL26Ek-X-0ipKbgdKTbWvEeOByXO2QL6cE";
    let y = "9dloNwMe1N7IYsQUBzxwhBW0MMUR7hzGuZ4Qhkh3Wmg";
    let bearer = "eyJhbGciOiJFUzI1NiIsInR5cCI6IkpXVCIsImtpZCI6InRlc3Qta2lkIn0.eyJzdWIiOiIxMTExMTExMS0xMTExLTExMTEtMTExMS0xMTExMTExMTExMTEiLCJpc3MiOiJodHRwczovL2V4YW1wbGUuc3VwYWJhc2UuY28vYXV0aC92MSIsImV4cCI6NDAwMDAwMDAwMCwiZW1haWwiOiJhQGIuY28ifQ.1-jlvzKyXTavpVgffnxpKd5oKcxdli52HK2_qDGUGUAvHd14Ryi5Nc7o0FpF216yx-1t4zLucjv4sawuWRBmdA";
    assert!(verify_es256_components(bearer, x, y, "https://other.supabase.co/auth/v1").is_err());
}
