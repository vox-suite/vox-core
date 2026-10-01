use axum::http::{
    HeaderValue, Method,
    header::{AUTHORIZATION, CONTENT_TYPE},
};
use std::{sync::OnceLock, time::Duration};
use tower_http::cors::{AllowOrigin, CorsLayer};

const DEFAULT_ORIGINS: [&str; 1] = ["https://appassets.androidplatform.net"];
const MAX_AGE: Duration = Duration::from_secs(600);

fn allowed_origins() -> &'static [String] {
    static ORIGINS: OnceLock<Vec<String>> = OnceLock::new();
    ORIGINS.get_or_init(|| {
        let extra = std::env::var("VOX_CORS_ALLOWED_ORIGINS").unwrap_or_default();
        DEFAULT_ORIGINS
            .iter()
            .map(|origin| origin.to_string())
            .chain(
                extra
                    .split(',')
                    .map(str::trim)
                    .filter(|origin| !origin.is_empty() && *origin != "*")
                    .map(str::to_string),
            )
            .collect()
    })
}

pub fn origin_allowed(origin: &str) -> bool {
    allowed_origins().iter().any(|allowed| allowed == origin)
}

pub fn layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(|origin: &HeaderValue, _| {
            origin.to_str().is_ok_and(origin_allowed)
        }))
        .allow_methods([
            Method::GET,
            Method::POST,
            Method::PUT,
            Method::PATCH,
            Method::DELETE,
        ])
        .allow_headers([AUTHORIZATION, CONTENT_TYPE])
        .max_age(MAX_AGE)
}
