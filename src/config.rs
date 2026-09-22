/**
 * Application configuration and environment variable loading for Vox Core.
 */

#[derive(Clone, Debug)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub service_token: String,
    pub gemini_api_key: String,
    pub gemini_model: String,
    pub exa_api_key: String,
    pub google_maps_api_key: Option<String>,
    pub bridge_url: Option<String>,
    pub jev_api_key: Option<String>,
    pub jev_base_url: String,
    pub jev_enabled: bool,
    pub tts_provider: String,
    pub audit_admin_token: Option<String>,
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
        let jev_base_url = "https://api.typesafe.ai/v1/systemone".to_string();
        let tts_provider = get("VOX_TTS_PROVIDER")
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "elevenlabs".to_string());

        Ok(Self {
            bind_address: "0.0.0.0:3001".to_string(),
            database_url: non_empty(&get, "DATABASE_URL")?,
            redis_url: get("REDIS_URL").filter(|value| !value.trim().is_empty()),
            service_token: non_empty(&get, "VOX_CORE_SERVICE_TOKEN")?,
            gemini_api_key: non_empty(&get, "GEMINI_API_KEY")?,
            gemini_model: "gemini-3.5-flash-lite".to_string(),
            exa_api_key: non_empty(&get, "EXA_API_KEY")?,
            google_maps_api_key: get("GOOGLE_MAPS_API_KEY")
                .filter(|value| !value.trim().is_empty()),
            bridge_url: get("VOX_BRIDGE_URL").filter(|value| !value.trim().is_empty()),
            jev_api_key,
            jev_base_url,
            jev_enabled,
            tts_provider,
            audit_admin_token: get("VOX_AUDIT_ADMIN_TOKEN")
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
