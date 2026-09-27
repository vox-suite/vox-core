/**
* Administrative HTTP endpoints for inspecting runtime state.
*/
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
    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn from_token(token: Option<String>) -> Result<Option<Self>, redis::RedisError> {
        let env_url = std::env::var("REDIS_URL").ok();
        Self::from_token_with_url(
            env_url.as_deref().or(Some(crate::config::DEFAULT_REDIS_URL)),
            token,
        )
    }

    pub fn from_token_with_url(
        url: Option<&str>,
        token: Option<String>,
    ) -> Result<Option<Self>, redis::RedisError> {
        token
            .filter(|value| !value.trim().is_empty())
            .map(|token| Self::new(url.or(Some(crate::config::DEFAULT_REDIS_URL)), token))
            .transpose()
    }

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
    Unauthorized,
    Unavailable,
    Invalid,
    Busy,
    Missing,
    Conflict,
}

impl axum::response::IntoResponse for AdminError {
    fn into_response(self) -> axum::response::Response {
        let (status, message) = match self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "Unauthorized."),
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Redis is unavailable. Try again later.",
            ),
            Self::Invalid => (StatusCode::BAD_REQUEST, "Invalid Redis query."),
            Self::Busy => (
                StatusCode::TOO_MANY_REQUESTS,
                "Redis explorer is busy. Try again shortly.",
            ),
            Self::Missing => (StatusCode::NOT_FOUND, "The Redis entry no longer exists."),
            Self::Conflict => (
                StatusCode::CONFLICT,
                "The Redis entry type changed. Refresh it before editing.",
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

#[derive(Deserialize)]
pub struct RedisUpdate {
    key: String,
    #[serde(rename = "type")]
    kind: String,
    value: JsonValue,
}

fn authorized(state: &AppState, headers: &HeaderMap) -> Result<Arc<RedisAdmin>, AdminError> {
    let admin = state.admin.clone().ok_or(AdminError::Unauthorized)?;
    if admin.token.trim().is_empty() || !auth::authorized(headers, &admin.token) {
        return Err(AdminError::Unauthorized);
    }
    Ok(admin)
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 1024 && !key.chars().any(char::is_control)
}

pub async fn browse(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedisQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let admin = match authorized(&state, &headers) {
        Ok(admin) => admin,
        Err(error) => return error.into_response(),
    };
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
    if query.key.as_ref().is_some_and(|key| !valid_key(key)) {
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

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<RedisQuery>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let admin = match authorized(&state, &headers) {
        Ok(admin) => admin,
        Err(error) => return error.into_response(),
    };
    let Some(key) = query.key.filter(|key| valid_key(key)) else {
        return AdminError::Invalid.into_response();
    };
    let result = tokio::time::timeout(Duration::from_secs(4), remove(admin, key)).await;
    match result {
        Ok(Ok(value)) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => AdminError::Unavailable.into_response(),
    }
}

async fn remove(admin: Arc<RedisAdmin>, key: String) -> Result<JsonValue, AdminError> {
    let _permit = admin.permits.try_acquire().map_err(|_| AdminError::Busy)?;
    let mut connection = admin.connection().await?;
    let deleted: u64 = redis::cmd("DEL")
        .arg(&key)
        .query_async(&mut connection)
        .await
        .map_err(|_| AdminError::Unavailable)?;
    if deleted == 0 {
        return Err(AdminError::Missing);
    }
    Ok(json!({"deleted":true,"key":key}))
}

pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(update): Json<RedisUpdate>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let admin = match authorized(&state, &headers) {
        Ok(admin) => admin,
        Err(error) => return error.into_response(),
    };
    if !valid_key(&update.key) {
        return AdminError::Invalid.into_response();
    }
    let arguments = match mutation_arguments(&update.kind, &update.value) {
        Ok(arguments) => arguments,
        Err(error) => return error.into_response(),
    };
    let result =
        tokio::time::timeout(Duration::from_secs(4), replace(admin, update, arguments)).await;
    match result {
        Ok(Ok(value)) => ([(header::CACHE_CONTROL, "no-store")], Json(value)).into_response(),
        Ok(Err(error)) => error.into_response(),
        Err(_) => AdminError::Unavailable.into_response(),
    }
}

fn mutation_arguments(kind: &str, value: &JsonValue) -> Result<Vec<String>, AdminError> {
    if kind == "string" {
        let value = value.as_str().ok_or(AdminError::Invalid)?;
        if value.len() > 65_536 {
            return Err(AdminError::Invalid);
        }
        return Ok(vec![value.to_owned()]);
    }
    let values = value.as_array().ok_or(AdminError::Invalid)?;
    let paired = matches!(kind, "hash" | "zset");
    if !matches!(kind, "list" | "hash" | "set" | "zset")
        || values.is_empty()
        || values.len() > if paired { 2_000 } else { 1_000 }
        || paired && values.len() % 2 != 0
    {
        return Err(AdminError::Invalid);
    }
    let arguments: Vec<String> = values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = if kind == "zset" && index % 2 == 1 {
                match value {
                    JsonValue::Number(value) => value.to_string(),
                    JsonValue::String(value) if value.parse::<f64>().is_ok_and(f64::is_finite) => {
                        value.clone()
                    }
                    _ => return Err(AdminError::Invalid),
                }
            } else {
                value.as_str().ok_or(AdminError::Invalid)?.to_owned()
            };
            if value.len() > 65_536 {
                return Err(AdminError::Invalid);
            }
            Ok(value)
        })
        .collect::<Result<_, _>>()?;
    if arguments.iter().map(String::len).sum::<usize>() > 131_072 {
        return Err(AdminError::Invalid);
    }
    Ok(arguments)
}

async fn replace(
    admin: Arc<RedisAdmin>,
    update: RedisUpdate,
    arguments: Vec<String>,
) -> Result<JsonValue, AdminError> {
    let _permit = admin.permits.try_acquire().map_err(|_| AdminError::Busy)?;
    let mut connection = admin.connection().await?;
    let script = r#"
local actual = redis.call('TYPE', KEYS[1])['ok']
if actual == 'none' then return {0, actual} end
if actual ~= ARGV[1] then return {-1, actual} end
local ttl = redis.call('PTTL', KEYS[1])
redis.call('DEL', KEYS[1])
if ARGV[1] == 'string' then
  redis.call('SET', KEYS[1], ARGV[2])
elseif ARGV[1] == 'list' then
  for i = 2, #ARGV do redis.call('RPUSH', KEYS[1], ARGV[i]) end
elseif ARGV[1] == 'hash' then
  for i = 2, #ARGV, 2 do redis.call('HSET', KEYS[1], ARGV[i], ARGV[i + 1]) end
elseif ARGV[1] == 'set' then
  for i = 2, #ARGV do redis.call('SADD', KEYS[1], ARGV[i]) end
elseif ARGV[1] == 'zset' then
  for i = 2, #ARGV, 2 do redis.call('ZADD', KEYS[1], ARGV[i + 1], ARGV[i]) end
end
if ttl > 0 then redis.call('PEXPIRE', KEYS[1], ttl) end
return {1, actual}
"#;
    let mut command = redis::cmd("EVAL");
    command
        .arg(script)
        .arg(1)
        .arg(&update.key)
        .arg(&update.kind);
    for argument in arguments {
        command.arg(argument);
    }
    let (outcome, _actual): (i64, String) = command
        .query_async(&mut connection)
        .await
        .map_err(|_| AdminError::Unavailable)?;
    match outcome {
        1 => Ok(json!({"updated":true,"key":update.key})),
        0 => Err(AdminError::Missing),
        _ => Err(AdminError::Conflict),
    }
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
