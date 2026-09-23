/**
* HTTP client for interacting with the Vox voice bridge.
*/
use crate::identity::ChannelIdentity;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboundCallRequest {
    pub action_id: Uuid,
    pub identity: ChannelIdentity,
    pub reason: String,
    pub opening_instruction: String,
    pub conversation_id: Uuid,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutboundCallResponse {
    pub provider_call_id: String,
}

#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    #[error("http transport failure: {0}")]
    Http(#[from] reqwest::Error),
    #[error("bridge returned error status {status}: {message}")]
    Status {
        status: reqwest::StatusCode,
        message: String,
    },
    #[error("invalid bridge url: {0}")]
    InvalidUrl(String),
}

#[async_trait]
pub trait OutboundBridge: Send + Sync {
    async fn initiate_outbound_call(
        &self,
        request: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeError>;
}

#[derive(Clone)]
pub struct BridgeClient {
    http: reqwest::Client,
    base_url: String,
    service_token: String,
}

impl BridgeClient {
    pub fn new(base_url: String, service_token: String) -> Result<Self, BridgeError> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(10))
            .build()?;
        let trimmed_url = base_url.trim_end_matches('/').to_string();
        Ok(Self {
            http,
            base_url: trimmed_url,
            service_token,
        })
    }

    pub fn with_http(http: reqwest::Client, base_url: String, service_token: String) -> Self {
        let trimmed_url = base_url.trim_end_matches('/').to_string();
        Self {
            http,
            base_url: trimmed_url,
            service_token,
        }
    }
}

#[async_trait]
impl OutboundBridge for BridgeClient {
    async fn initiate_outbound_call(
        &self,
        request: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeError> {
        let url = format!("{}/internal/v1/actions/outbound-call", self.base_url);
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
                .unwrap_or_else(|_| "Unknown bridge error".to_string());
            return Err(BridgeError::Status { status, message });
        }

        let result = response.json::<OutboundCallResponse>().await?;
        Ok(result)
    }
}
