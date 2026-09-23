/**
* Memory projection consolidating user facts, tasks, and history.
*/
use crate::{db::Db, identity::UserId};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Serialize)]
struct UserContextProjection {
    current_time_utc: String,
    profile: Value,
    commitments: Vec<String>,
    decisions: Vec<String>,
    recent_recaps: Vec<String>,
}

pub async fn build(db: &Db, user_id: UserId) -> Result<String, sqlx::Error> {
    let (profile, rows) = tokio::try_join!(
        async {
            sqlx::query_scalar::<_, Value>("SELECT profile_facts FROM users WHERE id = $1")
                .bind(user_id.0)
                .fetch_optional(db.pool())
                .await
        },
        async {
            sqlx::query(
                "SELECT latest_summary->>'recap' AS recap, \
                        COALESCE(latest_summary->'commitments', '[]'::jsonb) AS commitments, \
                        COALESCE(latest_summary->'decisions', '[]'::jsonb) AS decisions \
                 FROM conversations \
                 WHERE user_id = $1 AND summary_version > 0 \
                 ORDER BY updated_at DESC LIMIT 50",
            )
            .bind(user_id.0)
            .fetch_all(db.pool())
            .await
        }
    )?;
    let profile = profile.unwrap_or_else(|| serde_json::json!({}));
    let mut projection = UserContextProjection {
        current_time_utc: chrono::Utc::now().to_rfc3339(),
        profile,
        commitments: rows
            .iter()
            .flat_map(|row| json_strings(row.get("commitments")))
            .collect(),
        decisions: rows
            .iter()
            .flat_map(|row| json_strings(row.get("decisions")))
            .collect(),
        recent_recaps: rows
            .iter()
            .filter_map(|row| row.get::<Option<String>, _>("recap"))
            .collect(),
    };
    bounded_json(&mut projection)
}

fn json_strings(value: Value) -> Vec<String> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.as_str().map(ToOwned::to_owned))
        .collect()
}

fn bounded_json(projection: &mut UserContextProjection) -> Result<String, sqlx::Error> {
    while !projection.recent_recaps.is_empty() {
        let serialized = serde_json::to_string(projection)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        if serialized.len() <= MAX_CONTEXT_BYTES {
            return Ok(serialized);
        }
        projection.recent_recaps.pop();
    }
    serde_json::to_string(projection).map_err(|error| sqlx::Error::Decode(Box::new(error)))
}
