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
    generation: u64,
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
    next_generation: Arc<std::sync::atomic::AtomicU64>,
    confirmations: Arc<Mutex<HashMap<Uuid, PendingCommand>>>,
}

/// A device command the agent proposed but the user has not yet confirmed.
/// `turn` identifies the agent turn that proposed it, so confirmation must
/// arrive in a later turn — i.e. after the user actually answered.
#[derive(Clone)]
struct PendingCommand {
    device_id: Uuid,
    command: String,
    turn: Uuid,
    expires_at: std::time::Instant,
}

const CONFIRMATION_TTL: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// No matching earlier proposal: this call has now been recorded as one.
    Proposed,
    /// Same turn that proposed it — the user has not answered yet.
    AwaitingUser,
    /// Proposed in an earlier turn and repeated now: run it.
    Confirmed,
}

impl DeviceHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a newly connected device, returning the connection
    /// generation (pass it back to `unregister`) and the receiver of outgoing
    /// frames (for the socket writer). A reconnect replaces the previous link;
    /// dropping its sender closes the old socket's writer.
    pub fn register(&self, device_id: Uuid) -> (u64, mpsc::UnboundedReceiver<String>) {
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        let link = DeviceLink {
            generation,
            to_device: tx,
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        self.conns
            .lock()
            .expect("device hub lock poisoned")
            .insert(device_id, link);
        (generation, rx)
    }

    /// Removes the link only if it is still the one `generation` registered,
    /// so a stale socket closing never drops a newer reconnect.
    pub fn unregister(&self, device_id: Uuid, generation: u64) {
        let mut conns = self.conns.lock().expect("device hub lock poisoned");
        if conns.get(&device_id).map(|link| link.generation) == Some(generation) {
            conns.remove(&device_id);
        }
    }

    /// Whether the user has an unexpired device command awaiting their
    /// answer, so the next turn (e.g. a bare "yes") keeps the device tools.
    pub fn has_pending_command(&self, user_id: Uuid) -> bool {
        self.confirmations
            .lock()
            .expect("device confirmation lock poisoned")
            .get(&user_id)
            .is_some_and(|p| p.expires_at > std::time::Instant::now())
    }

    /// Server-enforced human confirmation for a device command. The first
    /// call records a proposal; only a repeat of the exact same command from
    /// a *later* agent turn (after the user answered) is `Confirmed`.
    pub fn confirm_command(
        &self,
        user_id: Uuid,
        device_id: Uuid,
        command: &str,
        turn: Uuid,
    ) -> Confirmation {
        let now = std::time::Instant::now();
        let mut pending = self
            .confirmations
            .lock()
            .expect("device confirmation lock poisoned");
        pending.retain(|_, p| p.expires_at > now);
        match pending.get(&user_id) {
            Some(p) if p.device_id == device_id && p.command == command => {
                if p.turn == turn {
                    Confirmation::AwaitingUser
                } else {
                    pending.remove(&user_id);
                    Confirmation::Confirmed
                }
            }
            _ => {
                pending.insert(
                    user_id,
                    PendingCommand {
                        device_id,
                        command: command.to_owned(),
                        turn,
                        expires_at: now + CONFIRMATION_TTL,
                    },
                );
                Confirmation::Proposed
            }
        }
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

/// Registry of live per-user "your data changed" WebSocket connections.
/// Unlike [`DeviceHub`], this isn't gated behind device-control consent —
/// any signed-in user gets a connection — and it never expects a reply: a
/// notification just tells the client something changed so it can refetch
/// over the normal REST API. Best-effort only: if the user has no live
/// connection, a notify is silently dropped and the client picks up the
/// change next time it polls or reconnects.
///
/// A user can have several connections at once (desktop app + phone open
/// together), so each is tagged with the client-reported `platform`
/// (e.g. "macos-aarch64", "android") instead of one replacing another.
struct UserConnection {
    generation: u64,
    tx: mpsc::UnboundedSender<String>,
    platform: String,
}

#[derive(Clone, Default)]
pub struct UserEventHub {
    conns: Arc<Mutex<HashMap<Uuid, Vec<UserConnection>>>>,
    next_generation: Arc<std::sync::atomic::AtomicU64>,
}

impl UserEventHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a newly connected client for `user_id`, returning the
    /// connection generation (pass it back to `unregister`) and the receiver
    /// of outgoing frames. Additive: existing connections for the same user
    /// (other devices) are left alone.
    pub fn register(
        &self,
        user_id: Uuid,
        platform: impl Into<String>,
    ) -> (u64, mpsc::UnboundedReceiver<String>) {
        let generation = self
            .next_generation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (tx, rx) = mpsc::unbounded_channel();
        self.conns
            .lock()
            .expect("user event hub lock poisoned")
            .entry(user_id)
            .or_default()
            .push(UserConnection {
                generation,
                tx,
                platform: platform.into(),
            });
        (generation, rx)
    }

    /// Removes the link for `generation`, leaving the user's other
    /// connections untouched.
    pub fn unregister(&self, user_id: Uuid, generation: u64) {
        let mut conns = self.conns.lock().expect("user event hub lock poisoned");
        if let Some(list) = conns.get_mut(&user_id) {
            list.retain(|c| c.generation != generation);
            if list.is_empty() {
                conns.remove(&user_id);
            }
        }
    }

    /// Tells every one of `user_id`'s connected clients that something changed.
    pub fn notify(&self, user_id: Uuid, event: Value) {
        if let Some(list) = self
            .conns
            .lock()
            .expect("user event hub lock poisoned")
            .get(&user_id)
        {
            let frame = event.to_string();
            for conn in list {
                let _ = conn.tx.send(frame.clone());
            }
        }
    }

    /// Platforms `user_id` currently has a live connection from (e.g. to
    /// find whether their desktop app — and its local LLM — is reachable).
    pub fn platforms(&self, user_id: Uuid) -> Vec<String> {
        self.conns
            .lock()
            .expect("user event hub lock poisoned")
            .get(&user_id)
            .map(|list| list.iter().map(|c| c.platform.clone()).collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_unregister_keeps_newer_connection() {
        let hub = DeviceHub::new();
        let device = Uuid::new_v4();
        let (old, _rx_old) = hub.register(device);
        let (_new, _rx_new) = hub.register(device);
        hub.unregister(device, old);
        assert!(hub.get(device).is_some());
    }

    #[test]
    fn command_needs_confirmation_from_a_later_turn() {
        let hub = DeviceHub::new();
        let (user, device) = (Uuid::new_v4(), Uuid::new_v4());
        let (t1, t2) = (Uuid::new_v4(), Uuid::new_v4());
        assert_eq!(
            hub.confirm_command(user, device, "ls", t1),
            Confirmation::Proposed
        );
        assert_eq!(
            hub.confirm_command(user, device, "ls", t1),
            Confirmation::AwaitingUser
        );
        // A different command replaces the proposal instead of confirming.
        assert_eq!(
            hub.confirm_command(user, device, "rm -rf ~", t2),
            Confirmation::Proposed
        );
        assert_eq!(
            hub.confirm_command(user, device, "rm -rf ~", t2),
            Confirmation::AwaitingUser
        );
        assert!(hub.has_pending_command(user));
        let t3 = Uuid::new_v4();
        assert_eq!(
            hub.confirm_command(user, device, "rm -rf ~", t3),
            Confirmation::Confirmed
        );
        assert!(!hub.has_pending_command(user));
        // Consumed: running it again needs a fresh confirmation.
        assert_eq!(
            hub.confirm_command(user, device, "rm -rf ~", t3),
            Confirmation::Proposed
        );
    }
}
