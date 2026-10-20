use axum::{Json, extract::Query, http::StatusCode};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, utoipa::IntoParams)]
pub struct AppVersionQuery {
    pub platform: String,
    pub version: String,
}

#[derive(Serialize, utoipa::ToSchema)]
pub struct AppVersionStatus {
    pub status: &'static str,
    pub latest_version: Option<String>,
    pub download_url: Option<String>,
}

fn parts(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|part| part.parse().ok())
        .collect()
}

fn older(current: &str, target: &str) -> bool {
    let (current, target) = (parts(current), parts(target));
    let width = current.len().max(target.len());
    let pad = |mut v: Vec<u64>| {
        v.resize(width, 0);
        v
    };
    pad(current) < pad(target)
}

#[utoipa::path(get, path = "/v1/app/version", tag = "app", params(AppVersionQuery), responses((status = 200, body = AppVersionStatus), (status = 400)))]
pub async fn app_version(
    Query(q): Query<AppVersionQuery>,
) -> Result<Json<AppVersionStatus>, StatusCode> {
    let prefix = match q.platform.as_str() {
        "desktop" => "VOX_DESKTOP",
        "android" => "VOX_ANDROID",
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let env = |key: &str| {
        std::env::var(format!("{prefix}_{key}"))
            .ok()
            .filter(|value| !value.trim().is_empty())
    };
    let (minimum, latest) = (env("MIN_VERSION"), env("LATEST_VERSION"));
    let status = if minimum.as_deref().is_some_and(|min| older(&q.version, min)) {
        "update_required"
    } else if latest
        .as_deref()
        .is_some_and(|latest| older(&q.version, latest))
    {
        "update_available"
    } else {
        "ok"
    };
    Ok(Json(AppVersionStatus {
        status,
        latest_version: latest,
        download_url: env("DOWNLOAD_URL"),
    }))
}
