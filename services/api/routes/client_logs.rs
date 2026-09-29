use axum::{Extension, Json, http::StatusCode};
use serde::Deserialize;
use vox_core::domain::identity::Actor;

const MAX_LINES: usize = 500;
const MAX_MESSAGE_CHARS: usize = 4096;

#[derive(Deserialize)]
pub struct ClientLogLine {
    pub ts: String,
    pub level: String,
    pub tag: String,
    pub message: String,
}

#[derive(Deserialize)]
pub struct ClientLogBatch {
    pub platform: String,
    pub app_version: String,
    pub device_id: String,
    pub lines: Vec<ClientLogLine>,
}

pub async fn submit_client_logs(
    Extension(actor): Extension<Actor>,
    Json(body): Json<ClientLogBatch>,
) -> StatusCode {
    for line in body.lines.iter().take(MAX_LINES) {
        let message: String = line.message.chars().take(MAX_MESSAGE_CHARS).collect();
        println!(
            "[client-log] user={} {}/{} v{} {} {} {}: {}",
            actor.user_id,
            body.platform,
            body.device_id,
            body.app_version,
            line.ts,
            line.level,
            line.tag,
            message.replace('\n', "\\n"),
        );
    }
    StatusCode::NO_CONTENT
}
