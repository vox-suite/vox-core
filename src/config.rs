/*!
* Application configuration and environment variable loading for Vox Core.
*/
/// Infra defaults, overridable by the env vars read below. Centralized here
/// instead of scattered as literals in http/admin.rs, db/mod.rs, and the
/// service binaries.
pub const DEFAULT_BIND_ADDRESS: &str = "0.0.0.0:3001";
pub const DEFAULT_GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
pub const DEFAULT_JEV_BASE_URL: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_TTS_PROVIDER: &str = "elevenlabs";
pub const DEFAULT_REDIS_URL: &str = "redis://redis:6379";
pub const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
pub const DEFAULT_DB_ACQUIRE_TIMEOUT_SECS: u64 = 5;

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub db_max_connections: u32,
    pub db_acquire_timeout_secs: u64,
    pub redis_url: String,
    pub service_token: String,
    pub gemini_api_key: String,
    pub gemini_model: String,
    pub exa_api_key: String,
    pub google_maps_api_key: Option<String>,
    pub bridge_url: Option<String>,
    pub core_api_url: Option<String>,
    pub jev_api_key: Option<String>,
    pub jev_base_url: String,
    pub jev_enabled: bool,
    pub tts_provider: String,
    pub status_webhook_key: Option<String>,
    /// 32-byte hex key that encrypts connected-app OAuth tokens at rest.
    pub credential_key: Option<String>,
    /// Exact redirect URIs hosts may use for connected-app OAuth callbacks.
    pub mcp_oauth_redirect_uris: Vec<String>,
    /// JSON map of MCP endpoint host -> OAuth client, for apps without
    /// dynamic client registration.
    pub mcp_oauth_clients: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} is missing")]
    Missing(&'static str),
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    pub fn from_values<F>(get: F) -> Result<Self, ConfigError>
    where
        F: Fn(&str) -> Option<String>,
    {
        let jev_api_key = get("JEV_API_KEY").filter(|value| !value.trim().is_empty());
        let jev_enabled = jev_api_key.is_some();
        let jev_base_url = get("JEV_BASE_URL")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_JEV_BASE_URL.to_string());
        let tts_provider = get("VOX_TTS_PROVIDER")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TTS_PROVIDER.to_string());

        Ok(Self {
            bind_address: get("VOX_CORE_BIND_ADDRESS")
                .filter(|value| !value.trim().is_empty())
                // Railway (and similar PaaS) assign a container port via `PORT`
                // rather than letting the app pick its own bind address.
                .or_else(|| get("PORT").map(|p| format!("0.0.0.0:{p}")))
                .unwrap_or_else(|| DEFAULT_BIND_ADDRESS.to_string()),
            database_url: non_empty(&get, "DATABASE_URL")?,
            db_max_connections: get("DB_MAX_CONNECTIONS")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_DB_MAX_CONNECTIONS),
            db_acquire_timeout_secs: get("DB_ACQUIRE_TIMEOUT_SECS")
                .and_then(|value| value.parse().ok())
                .unwrap_or(DEFAULT_DB_ACQUIRE_TIMEOUT_SECS),
            redis_url: get("REDIS_URL")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_REDIS_URL.to_string()),
            service_token: non_empty(&get, "VOX_AUTH_TOKEN")?,
            gemini_api_key: non_empty(&get, "GEMINI_API_KEY")?,
            gemini_model: get("GEMINI_MODEL")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_GEMINI_MODEL.to_string()),
            exa_api_key: non_empty(&get, "EXA_API_KEY")?,
            google_maps_api_key: get("GOOGLE_MAPS_API_KEY")
                .filter(|value| !value.trim().is_empty()),
            bridge_url: get("VOX_BRIDGE_URL").filter(|value| !value.trim().is_empty()),
            core_api_url: get("VOX_CORE_API_URL").filter(|value| !value.trim().is_empty()),
            jev_api_key,
            jev_base_url,
            jev_enabled,
            tts_provider,
            status_webhook_key: get("VOX_STATUS_WEBHOOK_KEY")
                .filter(|value| !value.trim().is_empty()),
            credential_key: get("VOX_CREDENTIAL_KEY").filter(|value| !value.trim().is_empty()),
            mcp_oauth_redirect_uris: get("VOX_MCP_OAUTH_REDIRECT_URIS")
                .map(|value| {
                    value
                        .split(',')
                        .map(|uri| uri.trim().to_string())
                        .filter(|uri| !uri.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            mcp_oauth_clients: get("VOX_MCP_OAUTH_CLIENTS")
                .filter(|value| !value.trim().is_empty()),
        })
    }
}

fn non_empty<F>(get: &F, name: &'static str) -> Result<String, ConfigError>
where
    F: Fn(&str) -> Option<String>,
{
    get(name)
        .filter(|value| !value.trim().is_empty())
        .ok_or(ConfigError::Missing(name))
}
