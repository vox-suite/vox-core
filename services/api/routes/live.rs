/**
* Real-time WebSocket connection pushing "your data changed" signals to the
* current user's own clients (e.g. the desktop app), so a task created or
* updated elsewhere (a phone call, another device) shows up without waiting
* on the client's next poll.
*/
use axum::{
    Extension,
    extract::{
        State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    response::IntoResponse,
};
use futures_util::{SinkExt, StreamExt};
use vox_core::{domain::identity::Actor, realtime::UserEventHub};

#[derive(Clone)]
pub struct LiveApiState {
    pub hub: UserEventHub,
}

pub async fn live_socket(
    State(state): State<LiveApiState>,
    Extension(actor): Extension<Actor>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_socket(socket, state, actor.user_id))
}

const PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(20);

async fn handle_socket(
    socket: WebSocket,
    state: LiveApiState,
    user_id: uuid::Uuid,
) {
    tracing::info!(%user_id, "Live socket connected");
    let (generation, mut outgoing) = state.hub.register(user_id);
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
