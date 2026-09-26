use axum::{
    Json,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};
use serde_json::json;
/**
* Rate limiting middleware and IP tracking for HTTP endpoints.
*/
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

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
    if let Some(forwarded) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = forwarded.split(',').next().map(|s| s.trim())
        && !first.is_empty()
    {
        return first.to_string();
    }
    if let Some(real_ip) = headers.get("x-real-ip").and_then(|v| v.to_str().ok()) {
        let trimmed = real_ip.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(cf_ip) = headers
        .get("cf-connecting-ip")
        .and_then(|v| v.to_str().ok())
    {
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

        if clients.len() > 10_000
            && let Some(cutoff) = now.checked_sub(self.config.window)
        {
            clients.retain(|_, state| state.last_seen > cutoff);
        }

        let entry = clients
            .entry(key.to_string())
            .or_insert_with(|| ClientState {
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
