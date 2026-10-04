//! Outbound connector response filtering.
use serde_json::Value;

/// Reject credential-like content before returning external data to an agent.
pub fn scan_for_prohibited_content(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => {
            for (key, nested) in map {
                let lower = key.to_ascii_lowercase();
                if lower.contains("secret")
                    || lower.contains("password")
                    || lower.contains("token")
                    || (lower.contains("credential") && key != "credential_custody")
                    || lower.contains("private_key")
                    || lower.contains("session_id")
                    || lower.contains("cvv")
                    || lower.contains("card_number")
                    || lower.contains("pin")
                    || (lower.contains("approval") && !lower.contains("summary"))
                {
                    return Err(format!("prohibited key '{key}' detected in export"));
                }
                scan_for_prohibited_content(nested)?;
            }
        }
        Value::Array(items) => {
            for item in items {
                scan_for_prohibited_content(item)?;
            }
        }
        Value::String(value) => {
            let lower = value.to_ascii_lowercase();
            if lower.contains("sk_live_")
                || lower.contains("vox_sk_")
                || lower.contains("bearer ")
                || lower.contains("-----begin")
                || lower.contains("private key-----")
                || lower
                    .split(|c: char| {
                        c.is_whitespace() || c == '"' || c == '\'' || c == ',' || c == ';'
                    })
                    .any(|word| {
                        word.starts_with("sk-")
                            || word.starts_with("eyj")
                            || word.starts_with("key-")
                    })
            {
                return Err("credential or token value detected in export payload".into());
            }
        }
        _ => {}
    }
    Ok(())
}
