/**
* Authentication extractors and middleware for HTTP endpoints.
*/
use axum::http::HeaderMap;
use subtle::ConstantTimeEq;

pub fn authorized(headers: &HeaderMap, expected: &str) -> bool {
    let Some(value) = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return false;
    };
    bool::from(value.as_bytes().ct_eq(expected.as_bytes()))
}
