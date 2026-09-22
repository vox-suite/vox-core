/**
* Rate limiting middleware and IP tracking for HTTP endpoints.
*/
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use axum::{
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Clone, Debug)]
pub struct RateLimitConfig {
    pub max_requests: usize,
    pub window: Duration,
}

impl Default for RateLimitConfig {
    fn default() -> Self {
        let max_requests = std::env::var("RATE_LIMIT_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(100);
        let window_secs = std::env::var("RATE_LIMIT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        Self {
            max_requests,
            window: Duration::from_secs(window_secs),
        }
    }
}

pub fn client_ip(headers: &HeaderMap) -> String {
    if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = forwarded.split(',').next().map(|s| s.trim()) {
            if !first.is_empty() {
                return first.to_string();
            }
        }
    }
    if let Some(real_ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = real_ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(cf_ip) = headers.get("cf-connecting-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = cf_ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "127.0.0.1".to_string()
}

struct ClientState {
    timestamps: VecDeque<Instant>,
    last_seen: Instant,
}

#[derive(Clone)]
pub struct RateLimiter {
    config: RateLimitConfig,
    clients: Arc<Mutex<HashMap<String, ClientState>>>,
}

pub struct RateLimitDecision {
    pub allowed: bool,
    pub limit: usize,
    pub remaining: usize,
    pub reset_unix: u64,
    pub retry_after_secs: u64,
}

impl RateLimiter {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn check(&self, key: &str) -> RateLimitDecision {
        let now = Instant::now();
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let window_secs = self.config.window.as_secs().max(1);

        let mut clients = self.clients.lock().unwrap();

        if clients.len() > 10_000 {
            if let Some(cutoff) = now.checked_sub(self.config.window) {
                clients.retain(|_, state| state.last_seen > cutoff);
            }
        }

        let entry = clients.entry(key.to_string()).or_insert_with(|| ClientState {
            timestamps: VecDeque::new(),
            last_seen: now,
        });

        entry.last_seen = now;

        if let Some(cutoff) = now.checked_sub(self.config.window) {
            while let Some(&t) = entry.timestamps.front() {
                if t <= cutoff {
                    entry.timestamps.pop_front();
                } else {
                    break;
                }
            }
        }

        let count = entry.timestamps.len();
        if count < self.config.max_requests {
            entry.timestamps.push_back(now);
            let remaining = self.config.max_requests - (count + 1);
            let reset_unix = now_unix + window_secs;

            RateLimitDecision {
                allowed: true,
                limit: self.config.max_requests,
                remaining,
                reset_unix,
                retry_after_secs: 0,
            }
        } else {
            let oldest = entry.timestamps.front().copied().unwrap_or(now);
            let elapsed_since_oldest = now.saturating_duration_since(oldest);
            let remaining_in_window = self.config.window.saturating_sub(elapsed_since_oldest);
            let retry_after_secs = remaining_in_window.as_secs().max(1);
            let reset_unix = now_unix + retry_after_secs;

            RateLimitDecision {
                allowed: false,
                limit: self.config.max_requests,
                remaining: 0,
                reset_unix,
                retry_after_secs,
            }
        }
    }

    pub fn clear(&self) {
        let mut clients = self.clients.lock().unwrap();
        clients.clear();
    }
}

pub async fn rate_limit_middleware(
    State(limiter): State<RateLimiter>,
    request: Request,
    next: Next,
) -> Response {
    let ip = client_ip(request.headers());
    let decision = limiter.check(&ip);

    if decision.allowed {
        let mut response = next.run(request).await;
        let headers = response.headers_mut();
        if let Ok(val) = HeaderValue::from_str(&decision.limit.to_string()) {
            headers.insert("X-RateLimit-Limit", val);
        }
        if let Ok(val) = HeaderValue::from_str(&decision.remaining.to_string()) {
            headers.insert("X-RateLimit-Remaining", val);
        }
        if let Ok(val) = HeaderValue::from_str(&decision.reset_unix.to_string()) {
            headers.insert("X-RateLimit-Reset", val);
        }
        response
    } else {
        let body = Json(json!({
            "error": "Too many requests. Please try again later."
        }));
        let mut response = (StatusCode::TOO_MANY_REQUESTS, body).into_response();
        let headers = response.headers_mut();
        if let Ok(val) = HeaderValue::from_str(&decision.retry_after_secs.to_string()) {
            headers.insert(header::RETRY_AFTER, val);
        }
        if let Ok(val) = HeaderValue::from_str(&decision.limit.to_string()) {
            headers.insert("X-RateLimit-Limit", val);
        }
        if let Ok(val) = HeaderValue::from_str("0") {
            headers.insert("X-RateLimit-Remaining", val);
        }
        if let Ok(val) = HeaderValue::from_str(&decision.reset_unix.to_string()) {
            headers.insert("X-RateLimit-Reset", val);
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn extracts_client_ip_with_precedence_and_splitting() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), "127.0.0.1");

        headers.insert(
            "cf-connecting-ip",
            HeaderValue::from_static("104.28.19.42"),
        );
        assert_eq!(client_ip(&headers), "104.28.19.42");

        headers.insert(
            "x-real-ip",
            HeaderValue::from_static("192.0.2.1"),
        );
        assert_eq!(client_ip(&headers), "192.0.2.1");

        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.195, 70.41.3.18, 150.172.238.178"),
        );
        assert_eq!(client_ip(&headers), "203.0.113.195");
    }

    #[test]
    fn trims_whitespace_in_extracted_ip() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-real-ip",
            HeaderValue::from_static("   198.51.100.99  "),
        );
        assert_eq!(client_ip(&headers), "198.51.100.99");
    }

    #[test]
    fn limiter_reset_and_clear_cleans_stored_keys() {
        let limiter = RateLimiter::new(RateLimitConfig {
            max_requests: 1,
            window: Duration::from_secs(60),
        });
        let d1 = limiter.check("1.2.3.4");
        assert!(d1.allowed);

        let d2 = limiter.check("1.2.3.4");
        assert!(!d2.allowed);

        limiter.clear();
        let d3 = limiter.check("1.2.3.4");
        assert!(d3.allowed);
    }
}
