/**
* Real-time WebSocket connection pushing "your data changed" signals to the
* current user's own clients (e.g. the desktop app), so a task created or
* updated elsewhere (a phone call, another device) shows up without waiting
* on the client's next poll.
*/
use axum::{
    Extension,
    extract::{
        Query, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use sqlx::PgPool;
use vox_core::{
    domain::{devices::local_llm_capable, identity::Actor},
    realtime::UserEventHub,
};

#[derive(Clone)]
pub struct LiveApiState {
    pub hub: UserEventHub,
    pub pool: PgPool,
}

#[derive(Deserialize)]
pub struct LiveSocketQuery {
    /// Client-reported platform, e.g. "macos-aarch64" or "android". Same
    /// vocabulary as device registration's `platform` field.
    platform: Option<String>,
}

pub async fn live_socket(
    State(state): State<LiveApiState>,
    Extension(actor): Extension<Actor>,
    Query(query): Query<LiveSocketQuery>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    let platform = query.platform.unwrap_or_else(|| "unknown".to_string());
    // Best-effort: reuses the capabilities the client already reported when
    // it registered as a device, so we don't need a separate declaration here.
    let local_llm = sqlx::query_scalar::<_, serde_json::Value>(
        "SELECT capabilities FROM devices \
         WHERE user_id = $1 AND platform = $2 \
         ORDER BY last_seen_at DESC LIMIT 1",
    )
    .bind(actor.user_id)
    .bind(&platform)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten()
    .is_some_and(|capabilities| local_llm_capable(&capabilities));
    ws.on_upgrade(move |socket| handle_socket(socket, state, actor.user_id, platform, local_llm))
}

const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

async fn handle_socket(
    socket: WebSocket,
    state: LiveApiState,
    user_id: uuid::Uuid,
    platform: String,
    local_llm_capable: bool,
) {
    tracing::info!(%user_id, %platform, local_llm_capable, "Live socket connected");
    let (generation, mut outgoing) = state.hub.register(user_id, platform);
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

    // This socket is push-only; drain (and ignore) anything the client sends
    // purely to detect the read side closing.
    loop {
        tokio::select! {
            message = receiver.next() => {
                if message.is_none() || matches!(message, Some(Err(_))) {
                    break;
                }
            }
            _ = &mut forward_task => break,
        }
    }

    forward_task.abort();
    state.hub.unregister(user_id, generation);
    tracing::info!(%user_id, "Live socket disconnected");
}
