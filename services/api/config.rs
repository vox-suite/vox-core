/**
* Configuration loader and environment settings for the API service.
*/
use vox_core::config::Config;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct ApiConfig {
    pub inner: Config,
}

#[allow(dead_code)]
impl ApiConfig {
    pub fn from_env() -> Result<Self, String> {
        let inner = Config::from_env().map_err(|e| format!("Invalid configuration: {}", e))?;
        Ok(Self { inner })
    }
}
