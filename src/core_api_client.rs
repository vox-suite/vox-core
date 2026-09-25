use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct DispatchDeviceRequest {
    pub user_id: Uuid,
    pub capability: String,
    pub kind: String,
    pub params: Value,
    pub timeout_secs: u64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct DispatchDeviceResponse {
    pub result: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum CoreApiError {
    #[error("http transport failure: {0}")]
    Http(#[from] reqwest::Error),
    #[error("core api returned error status {status}: {message}")]
    Status {
        status: reqwest::StatusCode,
        message: String,
    },
}

#[async_trait]
pub trait DeviceDispatcher: Send + Sync {
    async fn dispatch(&self, request: DispatchDeviceRequest) -> Result<Value, CoreApiError>;
}

#[derive(Clone)]
pub struct CoreApiClient {
    http: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl CoreApiClient {
    pub fn new(base_url: String, service_token: String) -> Result<Self, CoreApiError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(35))
            .build()?;
        Ok(Self {
            http,
            base_url: base_url.trim_end_matches('/').to_string(),
            service_token,
        })
    }
}

#[async_trait]
impl DeviceDispatcher for CoreApiClient {
    async fn dispatch(&self, request: DispatchDeviceRequest) -> Result<Value, CoreApiError> {
        let url = format!("{}/internal/v1/devices/dispatch", self.base_url);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.service_token)
            .json(&request)
            .send()
            .await?;

        let status = response.status();
        if !status.is_success() {
            let message = response
                .text()
                .await
                .unwrap_or_else(|_| "Unknown core API error".to_string());
            return Err(CoreApiError::Status { status, message });
        }

        let result = response.json::<DispatchDeviceResponse>().await?;
        Ok(result.result)
    }
}
