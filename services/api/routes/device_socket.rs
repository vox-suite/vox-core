/**
* Real-time WebSocket connection for a registered client device, used to
* dispatch live commands (e.g. terminal control) and receive their results.
*/
use axum::{
    Extension,
    extract::{
        Path, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use vox_core::{
    domain::{devices::local_llm_capable, identity::Actor},
    realtime::DeviceHub,
};

#[derive(Clone)]
pub struct DeviceSocketState {
    pub hub: DeviceHub,
    pub pool: PgPool,
}

pub async fn device_socket(
    State(state): State<DeviceSocketState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let device = sqlx::query(
        "SELECT platform, capabilities FROM devices \
         WHERE id = $1 AND user_id = $2 AND is_active = true AND execution_consent = true",
    )
    .bind(id)
    .bind(actor.user_id)
    .fetch_optional(&state.pool)
    .await
    .unwrap_or(None);

    let Some(device) = device else {
        tracing::warn!(device_id = %id, user_id = %actor.user_id, "Device socket rejected: not an active device owned by this user");
        return axum::http::StatusCode::NOT_FOUND.into_response();
    };
    let platform: String = device.get("platform");
    let capabilities: serde_json::Value = device.get("capabilities");
    let local_llm = local_llm_capable(&capabilities);

    ws.on_upgrade(move |socket| handle_socket(socket, state, id, platform, local_llm))
}

const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

async fn handle_socket(
    socket: WebSocket,
    state: DeviceSocketState,
    device_id: Uuid,
    platform: String,
    local_llm_capable: bool,
) {
    tracing::info!(device_id = %device_id, %platform, local_llm_capable, "Device socket connected");
    let (generation, mut outgoing) = state.hub.register(device_id);
    let (mut sender, mut receiver) = socket.split();

    // Pings keep proxies from dropping an idle link; the writer ends when a
    // reconnect replaces this link (its sender is dropped) or a send fails.
    let mut forward_task = tokio::spawn(async move {
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await;
        loop {
            let message = tokio::select! {
                frame = outgoing.recv() => match frame {
                    Some(frame) => Message::Text(frame.into()),
                    None => break,
                },
                _ = ping.tick() => Message::Ping(Vec::new().into()),
            };
            if sender.send(message).await.is_err() {
                break;
            }
        }
        let _ = sender.close().await;
    });

    loop {
        tokio::select! {
            message = receiver.next() => {
                let Some(Ok(message)) = message else { break };
                let Message::Text(text) = message else { continue };
                if let Ok(frame) = serde_json::from_str::<serde_json::Value>(&text) {
                    state.hub.resolve_incoming(device_id, &frame);
                }
            }
            _ = &mut forward_task => break,
        }
    }

    forward_task.abort();
    state.hub.unregister(device_id, generation);
    tracing::info!(device_id = %device_id, "Device socket disconnected");
}
