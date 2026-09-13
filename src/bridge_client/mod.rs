use crate::identity::ChannelIdentity;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;
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
pub enum BridgeClientError {
    #[error("bridge request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("bridge returned error: status {0}")]
    Status(u16),
    #[error("bridge client not configured")]
    NotConfigured,
}

#[async_trait]
pub trait BridgeDispatch: Send + Sync {
    async fn initiate_outbound_call(
        &self,
        request: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeClientError>;
}

pub struct BridgeClient {
    client: reqwest::Client,
    endpoint: String,
    service_token: String,
}

impl BridgeClient {
    pub fn new(base_url: String, service_token: String) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()?;
        let endpoint = format!(
            "{}/internal/v1/actions/outbound-call",
            base_url.trim_end_matches('/')
        );
        Ok(Self {
            client,
            endpoint,
            service_token,
        })
    }
}

#[async_trait]
impl BridgeDispatch for BridgeClient {
    async fn initiate_outbound_call(
        &self,
        request: OutboundCallRequest,
    ) -> Result<OutboundCallResponse, BridgeClientError> {
        let response = self
            .client
            .post(&self.endpoint)
            .bearer_auth(&self.service_token)
            .json(&request)
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(BridgeClientError::Status(response.status().as_u16()));
        }

        let body: OutboundCallResponse = response.json().await?;
        Ok(body)
    }
}
