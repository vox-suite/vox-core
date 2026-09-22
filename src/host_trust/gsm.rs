/**
* Google Secret Manager access for durable host credentials.
*/
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct StoredCredential {
    pub credential_id: Uuid,
    pub deployment_id: Uuid,
    pub host_app_id: Uuid,
    pub deployment_external_key: String,
    pub host_app_external_key: String,
    pub secret: String,
    pub allowed_origins: Vec<String>,
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
struct MetadataToken {
    access_token: String,
    expires_in: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct SecretAccess {
    payload: Option<SecretPayload>,
}

#[derive(Debug, Deserialize)]
struct SecretPayload {
    data: Option<String>,
}

struct CachedToken {
    token: String,
    expires_at: Instant,
}

static ACCESS_TOKEN: std::sync::Mutex<Option<CachedToken>> = std::sync::Mutex::new(None);

pub(super) async fn read_credentials(
    resource: &str,
) -> Result<Vec<StoredCredential>, String> {
    let token = access_token().await?;
    let name = access_name(resource);
    let response = reqwest::Client::new()
        .get(format!("https://secretmanager.googleapis.com/v1/{name}:access"))
        .bearer_auth(token)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().as_u16() == 404 {
        return Ok(Vec::new());
    }
    if !response.status().is_success() {
        return Err(format!("secret manager access failed: {}", response.status()));
    }
    let body: SecretAccess = response.json().await.map_err(|error| error.to_string())?;
    let Some(data) = body.payload.and_then(|payload| payload.data) else {
        return Ok(Vec::new());
    };
    if data.trim().is_empty() {
        return Ok(Vec::new());
    }
    let bytes = STANDARD
        .decode(data.trim())
        .map_err(|error| error.to_string())?;
    serde_json::from_slice(&bytes).map_err(|error| error.to_string())
}

pub(super) async fn write_credentials(
    resource: &str,
    credentials: &[StoredCredential],
) -> Result<(), String> {
    let token = access_token().await?;
    let payload = serde_json::to_vec(credentials).map_err(|error| error.to_string())?;
    let encoded = STANDARD.encode(payload);
    let response = reqwest::Client::new()
        .post(format!(
            "https://secretmanager.googleapis.com/v1/{}:addVersion",
            secret_name(resource)
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "payload": { "data": encoded } }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("secret manager write failed: {}", response.status()))
    }
}

fn secret_name(resource: &str) -> String {
    resource
        .trim()
        .trim_end_matches('/')
        .trim_end_matches("/versions/latest")
        .to_string()
}

fn access_name(resource: &str) -> String {
    let name = secret_name(resource);
    if name.contains("/versions/") {
        name
    } else {
        format!("{name}/versions/latest")
    }
}

async fn access_token() -> Result<String, String> {
    if let Some(cached) = ACCESS_TOKEN.lock().ok().and_then(|guard| {
        guard
            .as_ref()
            .filter(|cached| cached.expires_at > Instant::now() + Duration::from_secs(30))
            .map(|cached| cached.token.clone())
    }) {
        return Ok(cached);
    }
    if let Ok(token) = std::env::var("GOOGLE_OAUTH_ACCESS_TOKEN") {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let (token, expires_in) = if let Ok(token) = metadata_token().await {
        token
    } else {
        service_account_token().await?
    };
    if let Ok(mut guard) = ACCESS_TOKEN.lock() {
        *guard = Some(CachedToken {
            token: token.clone(),
            expires_at: Instant::now() + Duration::from_secs(expires_in.saturating_sub(60).max(30)),
        });
    }
    Ok(token)
}

async fn metadata_token() -> Result<(String, u64), String> {
    let response = reqwest::Client::new()
        .get("http://metadata.google.internal/computeMetadata/v1/instance/service-accounts/default/token")
        .header("Metadata-Flavor", "Google")
        .timeout(Duration::from_millis(500))
        .send()
        .await
        .map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("metadata token failed: {}", response.status()));
    }
    let body: MetadataToken = response.json().await.map_err(|error| error.to_string())?;
    Ok((body.access_token, body.expires_in.unwrap_or(3600)))
}

async fn service_account_token() -> Result<(String, u64), String> {
    Err(
        "Google Secret Manager requires GOOGLE_OAUTH_ACCESS_TOKEN or the GCE metadata server"
            .to_string(),
    )
}
