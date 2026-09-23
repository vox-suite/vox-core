/**
* Application service managing device registrations and presence.
*/
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{devices::Device, identity::Actor},
    storage::devices::DeviceRepository,
};

#[derive(Debug, Deserialize)]
pub struct RegisterDeviceInput {
    pub device_identifier: String,
    pub platform: String,
    pub label: Option<String>,
    pub public_key: Option<String>,
    pub capabilities: Option<serde_json::Value>,
    pub execution_consent: Option<bool>,
}

#[derive(Clone)]
pub struct DeviceService {
    repo: DeviceRepository,
}

impl DeviceService {
    pub fn new(repo: DeviceRepository) -> Self {
        Self { repo }
    }

    pub async fn register_device(
        &self,
        actor: &Actor,
        input: RegisterDeviceInput,
    ) -> Result<Device, sqlx::Error> {
        self.repo
            .register(
                actor.user_id,
                &input.device_identifier,
                &input.platform,
                input.label.as_deref().unwrap_or(""),
                input.public_key.as_deref(),
                input.capabilities.unwrap_or_else(|| serde_json::json!({})),
                input.execution_consent.unwrap_or(false),
            )
            .await
    }

    pub async fn heartbeat(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        self.repo.heartbeat(actor.user_id, id).await
    }
}
