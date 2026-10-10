use std::path::{Path, PathBuf};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ObjectStoreError {
    #[error("invalid URI: {0}")]
    InvalidUri(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("path traversal rejected: {0}")]
    PathTraversal(String),
    #[error("payload exceeds limit of {0} bytes")]
    PayloadTooLarge(usize),
    #[error("object not found: {0}")]
    NotFound(String),
    #[error("storage remote error: {0}")]
    Remote(String),
    #[error("shared authenticated storage not configured: {0}")]
    Configuration(String),
}

pub fn is_explicit_dev_mode() -> bool {
    std::env::var("VOX_DEV_STORAGE")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        || std::env::var("VOX_ENV")
            .map(|v| v.eq_ignore_ascii_case("development") || v.eq_ignore_ascii_case("test"))
            .unwrap_or(false)
        || std::env::var("ENVIRONMENT")
            .map(|v| {
                v.eq_ignore_ascii_case("development")
                    || v.eq_ignore_ascii_case("test")
                    || v.eq_ignore_ascii_case("local")
            })
            .unwrap_or(false)
        || std::env::var("APP_ENV")
            .map(|v| {
                v.eq_ignore_ascii_case("development")
                    || v.eq_ignore_ascii_case("test")
                    || v.eq_ignore_ascii_case("local")
            })
            .unwrap_or(false)
}

pub struct SupabaseStorageConfig {
    pub url: String,
    pub key: String,
    pub bucket: String,
}

pub fn supabase_config() -> Option<SupabaseStorageConfig> {
    let url = std::env::var("SUPABASE_URL")
        .ok()
        .map(|s| s.trim().trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())?;

    let key = std::env::var("SUPABASE_SERVICE_ROLE_KEY")
        .or_else(|_| std::env::var("SUPABASE_SERVICE_KEY"))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())?;

    let bucket = std::env::var("SUPABASE_STORAGE_BUCKET")
        .unwrap_or_else(|_| "vox-attachments".to_string())
        .trim()
        .to_string();

    Some(SupabaseStorageConfig { url, key, bucket })
}

pub fn validate_configuration() -> Result<(), ObjectStoreError> {
    if let Some(config) = supabase_config() {
        let url = reqwest::Url::parse(&config.url)
            .map_err(|_| ObjectStoreError::Configuration("invalid Supabase URL".into()))?;
        if (!is_explicit_dev_mode() && url.scheme() != "https")
            || config.bucket.is_empty()
            || !config
                .bucket
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ObjectStoreError::Configuration(
                "invalid shared storage configuration".into(),
            ));
        }
        return Ok(());
    }
    if is_explicit_dev_mode() {
        return Ok(());
    }
    Err(ObjectStoreError::Configuration(
        "SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY are required".into(),
    ))
}

pub fn local_storage_root() -> PathBuf {
    if let Ok(dir) = std::env::var("VOX_STORAGE_PATH")
        && !dir.trim().is_empty()
    {
        return PathBuf::from(dir.trim());
    }
    PathBuf::from("data/storage")
}

const MAX_UPLOAD_BYTES: usize = 524_288_000;
const MAX_READ_BYTES: usize = MAX_UPLOAD_BYTES;

pub async fn store_object(
    user_id: Uuid,
    namespace: &str,
    filename_or_ext: &str,
    data: &[u8],
) -> Result<String, ObjectStoreError> {
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(ObjectStoreError::PayloadTooLarge(MAX_UPLOAD_BYTES));
    }

    let safe_ns = sanitize_segment(namespace);
    let obj_id = Uuid::new_v4();
    let ext = sanitize_extension(filename_or_ext);
    let filename = format!("{obj_id}{ext}");
    let object_key = format!("{user_id}/{safe_ns}/{filename}");

    if let Some(cfg) = supabase_config() {
        let endpoint = format!(
            "{}/storage/v1/object/{}/{}",
            cfg.url, cfg.bucket, object_key
        );
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;

        let res = client
            .post(&endpoint)
            .header("apikey", &cfg.key)
            .bearer_auth(&cfg.key)
            .header("content-type", "application/octet-stream")
            .header("x-upsert", "true")
            .body(data.to_vec())
            .send()
            .await
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;

        if !res.status().is_success() {
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(ObjectStoreError::Remote(format!(
                "Supabase storage upload failed ({status}): {body}"
            )));
        }

        return Ok(format!("vox-obj://{object_key}"));
    }

    if !is_explicit_dev_mode() {
        return Err(ObjectStoreError::Configuration(
            "production environment requires SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY for shared authenticated object storage; local storage requires explicit VOX_DEV_STORAGE=1 or VOX_ENV=development".into(),
        ));
    }

    let root = local_storage_root();
    let dir = root.join(user_id.to_string()).join(&safe_ns);
    tokio::fs::create_dir_all(&dir).await?;

    let full_path = dir.join(&filename);
    tokio::fs::write(&full_path, data).await?;

    Ok(format!("vox-obj://{object_key}"))
}

pub async fn read_object(uri: &str) -> Result<Vec<u8>, ObjectStoreError> {
    let object_key = parse_canonical_key(uri)?;

    if let Some(cfg) = supabase_config() {
        let endpoint = format!(
            "{}/storage/v1/object/authenticated/{}/{}",
            cfg.url, cfg.bucket, object_key
        );
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;

        let res = client
            .get(&endpoint)
            .header("apikey", &cfg.key)
            .bearer_auth(&cfg.key)
            .send()
            .await
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;

        if res.status().as_u16() == 404 {
            return Err(ObjectStoreError::NotFound(uri.to_string()));
        }
        if !res.status().is_success() {
            return Err(ObjectStoreError::Remote(format!(
                "Storage read failed: {}",
                res.status()
            )));
        }
        return bounded_response(res).await;
    }

    if !is_explicit_dev_mode() {
        return Err(ObjectStoreError::Configuration(
            "production environment requires SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY for shared authenticated object storage; local storage requires explicit VOX_DEV_STORAGE=1 or VOX_ENV=development".into(),
        ));
    }

    let path = resolve_local_path(&object_key)?;
    if !path.exists() {
        return Err(ObjectStoreError::NotFound(uri.to_string()));
    }
    if tokio::fs::metadata(&path).await?.len() > MAX_READ_BYTES as u64 {
        return Err(ObjectStoreError::PayloadTooLarge(MAX_READ_BYTES));
    }
    let bytes = tokio::fs::read(&path).await?;
    if bytes.len() > MAX_READ_BYTES {
        return Err(ObjectStoreError::PayloadTooLarge(MAX_READ_BYTES));
    }
    Ok(bytes)
}

pub async fn delete_object(uri: &str) -> Result<(), ObjectStoreError> {
    let object_key = parse_canonical_key(uri)?;

    if let Some(cfg) = supabase_config() {
        let endpoint = format!("{}/storage/v1/object/{}", cfg.url, cfg.bucket);
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;

        let response = client
            .delete(&endpoint)
            .header("apikey", &cfg.key)
            .bearer_auth(&cfg.key)
            .json(&serde_json::json!({ "prefixes": [object_key] }))
            .send()
            .await
            .map_err(|e| ObjectStoreError::Remote(e.to_string()))?;
        if !response.status().is_success() && response.status().as_u16() != 404 {
            return Err(ObjectStoreError::Remote(format!(
                "Storage deletion failed: {}",
                response.status()
            )));
        }

        return Ok(());
    }

    if !is_explicit_dev_mode() {
        return Err(ObjectStoreError::Configuration(
            "production environment requires SUPABASE_URL and SUPABASE_SERVICE_ROLE_KEY for shared authenticated object storage; local storage requires explicit VOX_DEV_STORAGE=1 or VOX_ENV=development".into(),
        ));
    }

    let path = resolve_local_path(&object_key)?;
    if path.exists() {
        tokio::fs::remove_file(&path).await?;
    }
    Ok(())
}

fn parse_canonical_key(uri: &str) -> Result<String, ObjectStoreError> {
    let stripped = uri
        .strip_prefix("vox-obj://")
        .ok_or_else(|| ObjectStoreError::InvalidUri(format!("expected vox-obj:// URI: {uri}")))?;

    let segments: Vec<&str> = stripped
        .split('/')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() != 3 || segments.join("/") != stripped {
        return Err(ObjectStoreError::InvalidUri(format!(
            "expected vox-obj://<user_id>/<namespace>/<file>: {uri}"
        )));
    }

    for seg in &segments {
        if *seg == ".."
            || seg.contains('/')
            || seg.contains('\\')
            || !seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        {
            return Err(ObjectStoreError::PathTraversal(uri.to_string()));
        }
    }

    let user_id = Uuid::parse_str(segments[0])
        .map_err(|_| ObjectStoreError::InvalidUri(format!("invalid user UUID in URI: {uri}")))?;

    let safe_ns = sanitize_segment(segments[1]);
    let safe_file = segments[2..].join("/");

    Ok(format!("{user_id}/{safe_ns}/{safe_file}"))
}

fn resolve_local_path(object_key: &str) -> Result<PathBuf, ObjectStoreError> {
    let root = local_storage_root();
    let mut resolved = root;
    for seg in object_key.split('/') {
        if seg == ".." {
            return Err(ObjectStoreError::PathTraversal(object_key.to_string()));
        }
        resolved.push(seg);
    }
    Ok(resolved)
}

fn sanitize_segment(s: &str) -> String {
    let clean: String = s
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if clean.is_empty() {
        "general".to_string()
    } else {
        clean
    }
}

fn sanitize_extension(s: &str) -> String {
    let p = Path::new(s);
    if let Some(ext) = p.extension().and_then(|e| e.to_str()) {
        let clean: String = ext.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if !clean.is_empty() {
            return format!(".{clean}");
        }
    }
    if s.starts_with('.') {
        let clean: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        if !clean.is_empty() {
            return format!(".{clean}");
        }
    }
    String::new()
}

async fn bounded_response(mut response: reqwest::Response) -> Result<Vec<u8>, ObjectStoreError> {
    if response
        .content_length()
        .is_some_and(|len| len > MAX_READ_BYTES as u64)
    {
        return Err(ObjectStoreError::PayloadTooLarge(MAX_READ_BYTES));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| ObjectStoreError::Remote(e.to_string()))?
    {
        if chunk.len() > MAX_READ_BYTES.saturating_sub(bytes.len()) {
            return Err(ObjectStoreError::PayloadTooLarge(MAX_READ_BYTES));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
