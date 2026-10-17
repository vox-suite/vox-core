/*!
* Application configuration and environment variable loading for Vox Core.
*/
/// Infra defaults, overridable by the env vars read below. Centralized here
/// instead of scattered as literals in http/admin.rs, db/mod.rs, and the
/// service binaries.
pub const DEFAULT_PORT: &str = "3001";
pub const GEMINI_MODEL: &str = "gemini-3.5-flash-lite";
pub const DEFAULT_JEV_BASE_URL: &str = "https://api.typesafe.ai/v1/systemone";
pub const TTS_PROVIDER: &str = "elevenlabs";
pub const DEFAULT_ELEVENLABS_VOICE_ID: &str = "21m00Tcm4TlvDq8ikWAM";
pub const ELEVENLABS_MODEL_ID: &str = "eleven_v4_turbo";
pub const DEFAULT_ELEVENLABS_OUTPUT_FORMAT: &str = "mp3_44100_128";
pub const DEFAULT_REDIS_URL: &str = "redis://redis:6379";
pub const DEFAULT_DB_MAX_CONNECTIONS: u32 = 10;
pub const DEFAULT_DB_ACQUIRE_TIMEOUT_SECS: u64 = 5;
pub const DEFAULT_SPACE_MAX_STEPS: usize = 60;
pub const DEFAULT_SPACE_MAX_CHILDREN: usize = 8;

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
    pub smtp_url: Option<String>,
    pub event_agent_enabled: bool,
    pub email_from: Option<String>,
    pub core_api_url: Option<String>,
    pub jev_api_key: Option<String>,
    pub jev_base_url: String,
    pub jev_enabled: bool,
    pub elevenlabs_api_key: Option<String>,
    pub elevenlabs_voice_id: String,
    pub elevenlabs_model_id: String,
    pub elevenlabs_output_format: String,
    pub assemblyai_api_key: Option<String>,
    pub status_webhook_key: Option<String>,
    /// 32-byte hex key that encrypts connected-app OAuth tokens at rest.
    pub credential_key: Option<String>,
    pub google_client_id: Option<String>,
    pub google_client_secret: Option<String>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{0} is missing")]
    Missing(&'static str),
    #[error("{0} is invalid")]
    Invalid(&'static str),
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

        Ok(Self {
            bind_address: format!("0.0.0.0:{}", positive_number::<u16, _>(&get, "PORT", 3001)?),
            database_url: non_empty(&get, "DATABASE_URL")?,
            db_max_connections: positive_number(
                &get,
                "DB_MAX_CONNECTIONS",
                DEFAULT_DB_MAX_CONNECTIONS,
            )?,
            db_acquire_timeout_secs: positive_number(
                &get,
                "DB_ACQUIRE_TIMEOUT_SECS",
                DEFAULT_DB_ACQUIRE_TIMEOUT_SECS,
            )?,
            redis_url: get("REDIS_URL")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_REDIS_URL.to_string()),
            service_token: non_empty(&get, "VOX_AUTH_TOKEN")?,
            gemini_api_key: non_empty(&get, "GEMINI_API_KEY")?,
            gemini_model: GEMINI_MODEL.to_string(),
            exa_api_key: non_empty(&get, "EXA_API_KEY")?,
            google_maps_api_key: get("GOOGLE_MAPS_API_KEY")
                .filter(|value| !value.trim().is_empty()),
            bridge_url: get("VOX_BRIDGE_URL").filter(|value| !value.trim().is_empty()),
            smtp_url: get("VOX_SMTP_URL").filter(|value| !value.trim().is_empty()),
            event_agent_enabled: match get("VOX_EVENT_AGENT_ENABLED") {
                None => true,
                Some(value) => match value.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" => true,
                    "false" | "0" => false,
                    _ => return Err(ConfigError::Invalid("VOX_EVENT_AGENT_ENABLED")),
                },
            },
            email_from: get("VOX_EMAIL_FROM").filter(|value| !value.trim().is_empty()),
            core_api_url: get("VOX_CORE_API_URL").filter(|value| !value.trim().is_empty()),
            jev_api_key,
            jev_base_url,
            jev_enabled,
            elevenlabs_api_key: get("ELEVENLABS_API_KEY").filter(|value| !value.trim().is_empty()),
            elevenlabs_voice_id: get("ELEVENLABS_VOICE_ID")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ELEVENLABS_VOICE_ID.to_string()),
            elevenlabs_model_id: ELEVENLABS_MODEL_ID.to_string(),
            elevenlabs_output_format: get("ELEVENLABS_OUTPUT_FORMAT")
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ELEVENLABS_OUTPUT_FORMAT.to_string()),
            assemblyai_api_key: get("ASSEMBLYAI_API_KEY").filter(|value| !value.trim().is_empty()),
            status_webhook_key: get("VOX_STATUS_WEBHOOK_KEY")
                .filter(|value| !value.trim().is_empty()),
            credential_key: get("VOX_CREDENTIAL_KEY").filter(|value| !value.trim().is_empty()),
            google_client_id: get("GOOGLE_CLIENT_ID").filter(|value| !value.trim().is_empty()),
            google_client_secret: get("GOOGLE_CLIENT_SECRET")
                .filter(|value| !value.trim().is_empty()),
        })
    }
}

fn positive_number<T, F>(get: &F, name: &'static str, default: T) -> Result<T, ConfigError>
where
    T: std::str::FromStr + Default + PartialOrd,
    F: Fn(&str) -> Option<String>,
{
    match get(name) {
        None => Ok(default),
        Some(value) => value
            .trim()
            .parse::<T>()
            .ok()
            .filter(|value| *value > T::default())
            .ok_or(ConfigError::Invalid(name)),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn configured(name: &str, value: &str) -> Result<Config, ConfigError> {
        Config::from_values(|key| match key {
            key if key == name => Some(value.into()),
            "DATABASE_URL" | "VOX_AUTH_TOKEN" | "GEMINI_API_KEY" | "EXA_API_KEY" => {
                Some("fixture".into())
            }
            _ => None,
        })
    }

    #[test]
    fn explicit_false_does_not_enable_autonomous_processing() {
        for value in ["false", "FALSE", " False ", "0"] {
            assert!(
                !configured("VOX_EVENT_AGENT_ENABLED", value)
                    .unwrap()
                    .event_agent_enabled
            );
        }
    }

    #[test]
    fn malformed_flags_and_pool_limits_are_rejected() {
        for (name, value) in [
            ("VOX_EVENT_AGENT_ENABLED", "disabled"),
            ("DB_MAX_CONNECTIONS", "oops"),
            ("DB_MAX_CONNECTIONS", "0"),
            ("DB_ACQUIRE_TIMEOUT_SECS", "0"),
            ("DB_ACQUIRE_TIMEOUT_SECS", ""),
            ("PORT", "0"),
            ("PORT", "not-a-port"),
        ] {
            assert!(configured(name, value).is_err(), "accepted invalid {name}");
        }
    }
}
