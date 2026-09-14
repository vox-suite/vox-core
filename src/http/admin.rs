use super::{AppState, auth};
use axum::{
    Json,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, header},
};
use redis::{Client, Value, aio::ConnectionManager};
use serde::{Deserialize, Serialize};
use serde_json::{Value as JsonValue, json};
use std::{sync::Arc, time::Duration};
use tokio::sync::{OnceCell, Semaphore};

pub struct RedisAdmin {
    token: String,
    client: Option<Client>,
    connection: OnceCell<ConnectionManager>,
    permits: Semaphore,
}

impl RedisAdmin {
    pub fn new(url: Option<&str>, token: String) -> Result<Self, redis::RedisError> {
        Ok(Self {
            token,
            client: url.map(Client::open).transpose()?,
            connection: OnceCell::new(),
            permits: Semaphore::new(8),
        })
    }
    async fn connection(&self) -> Result<ConnectionManager, AdminError> {
        let client = self.client.as_ref().ok_or(AdminError::Unavailable)?;
        self.connection
            .get_or_try_init(|| client.get_connection_manager())
            .await
            .cloned()
            .map_err(|_| AdminError::Unavailable)
    }
}

#[derive(Debug)]
enum AdminError {
    Unavailable,
    Invalid,
    Busy,
}
impl axum::response::IntoResponse for AdminError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Redis is unavailable. Try again later.",
            ),
            Self::Invalid => (StatusCode::BAD_REQUEST, "Invalid Redis query."),
            Self::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "Redis explorer is busy. Try again shortly.",
            ),
        };
        (
            status,
            [(header::CACHE_CONTROL, "no-store")],
            Json(json!({"error":message})),
        )
            .into_response()
    }
}

#[derive(Deserialize)]
pub struct RedisQuery {
    cursor: Option<String>,
    r#match: Option<String>,
    key: Option<String>,
}
#[derive(Serialize)]
struct Entry {
    key: String,
    r#type: String,
    ttl: i64,
}

pub async fn browse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedisQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let Some(admin) = state.admin else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response();
    };
    if admin.token.trim().is_empty() || !auth::authorized(&headers, &admin.token) {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::CACHE_CONTROL, "no-store")],
        )
            .into_response();
    }
    let result = tokio::time::timeout(Duration::from_secs(4), read(admin, query)).await;
    match result {
        Ok(Ok(value)) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => AdminError::Unavailable.into_response(),
    }
}

async fn read(admin: Arc<RedisAdmin>, query: RedisQuery) -> Result<JsonValue, AdminError> {
    let cursor = query.cursor.as_deref().unwrap_or("0");
    if cursor.len() > 20 || cursor.is_empty() || !cursor.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AdminError::Invalid);
    }
    let cursor = cursor.parse::<u64>().map_err(|_| AdminError::Invalid)?;
    let pattern = query.r#match.as_deref().unwrap_or("vox:*");
    if pattern.len() > 256 || pattern.chars().any(char::is_control) {
        return Err(AdminError::Invalid);
    }
    if query
        .key
        .as_ref()
        .is_some_and(|key| key.is_empty() || key.len() > 1024 || key.chars().any(char::is_control))
    {
        return Err(AdminError::Invalid);
    }
    let _permit = admin.permits.try_acquire().map_err(|_| AdminError::Busy)?;
    let mut connection = admin.connection().await?;
    if let Some(key) = query.key {
        let (kind, ttl, size, truncated, preview): (String, i64, u64, bool, Value) =
            redis::cmd("EVAL_RO")
                .arg(include_str!("redis_preview.lua"))
                .arg(1)
                .arg(&key)
                .query_async(&mut connection)
                .await
                .map_err(|_| AdminError::Unavailable)?;
        return Ok(
            json!({"key":key,"type":kind,"ttl":ttl,"size":size,"truncated":truncated,"value":as_json(preview)}),
        );
    }
    let (next_cursor, keys): (u64, Vec<String>) = redis::cmd("SCAN")
        .arg(cursor)
        .arg("MATCH")
        .arg(pattern)
        .arg("COUNT")
        .arg(100)
        .query_async(&mut connection)
        .await
        .map_err(|_| AdminError::Unavailable)?;
    let mut pipeline = redis::pipe();
    for key in &keys {
        pipeline.cmd("TYPE").arg(key).cmd("TTL").arg(key);
    }
    let metadata: Vec<Value> = if keys.is_empty() {
        vec![]
    } else {
        pipeline
            .query_async(&mut connection)
            .await
            .map_err(|_| AdminError::Unavailable)?
    };
    let entries: Vec<Entry> = keys
        .into_iter()
        .enumerate()
        .map(|(index, key)| {
            let kind = redis::from_redis_value::<String>(&metadata[index * 2])
                .unwrap_or_else(|_| "unknown".into());
            let ttl = redis::from_redis_value::<i64>(&metadata[index * 2 + 1]).unwrap_or(-2);
            Entry {
                key,
                r#type: kind,
                ttl,
            }
        })
        .collect();
    Ok(json!({"cursor":next_cursor.to_string(),"entries":entries,"match":pattern}))
}

fn as_json(value: Value) -> JsonValue {
    match value {
        Value::Nil => JsonValue::Null,
        Value::Int(value) => json!(value),
        Value::BulkString(value) => json!(String::from_utf8_lossy(&value)),
        Value::SimpleString(value) => json!(value),
        Value::Array(values) => JsonValue::Array(values.into_iter().map(as_json).collect()),
        _ => JsonValue::Null,
    }
}
