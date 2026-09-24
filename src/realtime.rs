/**
* In-memory registry of live device WebSocket connections and pending
* request/response correlation, used to dispatch real-time commands
* (e.g. terminal control) to a connected client device.
*/
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum DeviceLinkError {
    #[error("device is not connected")]
    NotConnected,
    #[error("device did not respond in time")]
    Timeout,
    #[error("device connection closed")]
    Disconnected,
}

#[derive(Clone)]
pub struct DeviceLink {
    to_device: mpsc::UnboundedSender<String>,
    pending: Arc<Mutex<HashMap<Uuid, oneshot::Sender<Value>>>>,
}

impl DeviceLink {
    /// Sends a JSON request to the device and awaits its correlated response,
    /// giving up after `timeout`.
    pub async fn request(
        &self,
        kind: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, DeviceLinkError> {
        let request_id = Uuid::new_v4();
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .expect("device pending lock poisoned")
            .insert(request_id, tx);

        let mut frame = params;
        if !frame.is_object() {
            frame = serde_json::json!({});
        }
        frame["id"] = serde_json::json!(request_id);
        frame["type"] = serde_json::json!(kind);

        if self.to_device.send(frame.to_string()).is_err() {
            self.pending
                .lock()
                .expect("device pending lock poisoned")
                .remove(&request_id);
            return Err(DeviceLinkError::Disconnected);
        }

        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(DeviceLinkError::Disconnected),
            Err(_) => {
                self.pending
                    .lock()
                    .expect("device pending lock poisoned")
                    .remove(&request_id);
                Err(DeviceLinkError::Timeout)
            }
        }
    }
}

/// Shared registry mapping a connected device id to its live link. One
/// process-wide instance is shared between the WebSocket route (which
/// registers/unregisters connections and resolves incoming responses) and
/// agent tools (which look up a device and send it requests).
#[derive(Clone, Default)]
pub struct DeviceHub {
    conns: Arc<Mutex<HashMap<Uuid, DeviceLink>>>,
}

impl DeviceHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a newly connected device, returning its link (for the tool
    /// side) and the receiver of outgoing frames (for the socket writer).
    pub fn register(&self, device_id: Uuid) -> mpsc::UnboundedReceiver<String> {
        let (tx, rx) = mpsc::unbounded_channel();
        let link = DeviceLink {
            to_device: tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        self.conns
            .lock()
            .expect("device hub lock poisoned")
            .insert(device_id, link);
        rx
    }

    pub fn unregister(&self, device_id: Uuid) {
        self.conns
            .lock()
            .expect("device hub lock poisoned")
            .remove(&device_id);
    }

    pub fn get(&self, device_id: Uuid) -> Option<DeviceLink> {
        self.conns
            .lock()
            .expect("device hub lock poisoned")
            .get(&device_id)
            .cloned()
    }

    /// Delivers an incoming frame from a device to whichever pending request
    /// it correlates with, identified by the frame's `id` field.
    pub fn resolve_incoming(&self, device_id: Uuid, frame: &Value) {
        let Some(request_id) = frame
            .get("id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return;
        };
        let link = self
            .conns
            .lock()
            .expect("device hub lock poisoned")
            .get(&device_id)
            .cloned();
        if let Some(link) = link
            && let Some(tx) = link
                .pending
                .lock()
                .expect("device pending lock poisoned")
                .remove(&request_id)
        {
            let _ = tx.send(frame.clone());
        }
    }
}
