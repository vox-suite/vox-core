#[derive(Clone, Debug)]
pub struct Config {
    pub bind_address: String,
    pub database_url: String,
    pub redis_url: Option<String>,
    pub service_token: String,
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
        Ok(Self {
            bind_address: non_empty(&get, "VOX_CORE_BIND_ADDRESS")?,
            database_url: non_empty(&get, "DATABASE_URL")?,
            redis_url: get("REDIS_URL").filter(|value| !value.trim().is_empty()),
            service_token: non_empty(&get, "VOX_CORE_SERVICE_TOKEN")?,
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
