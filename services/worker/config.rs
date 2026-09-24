/**
* Configuration loader and concurrency settings for the worker daemon.
*/
use vox_core::config::Config;

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct WorkerConfig {
    pub inner: Config,
}

#[allow(dead_code)]
impl WorkerConfig {
    pub fn from_env() -> Result<Self, String> {
        let inner =
            Config::from_env().map_err(|e| format!("Invalid worker configuration: {}", e))?;
        Ok(Self { inner })
    }
}
