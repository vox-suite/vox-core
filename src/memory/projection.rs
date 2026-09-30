/**
* Memory projection containing only one agent’s facts and scoped history.
*/
use crate::{db::Db, identity::ResourceOwner};
use serde::Serialize;
use serde_json::Value;
use sqlx::Row;

pub const MAX_CONTEXT_BYTES: usize = 16 * 1024;

#[derive(Serialize)]
struct AgentMemoryProjection {
    current_time_utc: String,
    facts: Value,
    commitments: Vec<String>,
    decisions: Vec<String>,
    recent_recaps: Vec<String>,
}

pub async fn build(db: &Db, owner: ResourceOwner, agent_key: &str) -> Result<String, sqlx::Error> {
    let mut tx = db.pool().begin().await?;
    let actor = super::active_actor(&mut tx, owner, agent_key).await?;
    let profile = sqlx::query_scalar::<_, Value>(
        "SELECT facts FROM agent_memories WHERE user_context_id=$1 AND agent_id=$2",
    )
    .bind(owner.user_context_id.0)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?
    .unwrap_or_else(|| serde_json::json!({}));
    let rows = sqlx::query(
        "SELECT LEFT(latest_summary->>'recap',4096) AS recap, \
         CASE WHEN octet_length((latest_summary->'commitments')::text)<=8192 THEN latest_summary->'commitments' ELSE '[]'::jsonb END AS commitments, \
         CASE WHEN octet_length((latest_summary->'decisions')::text)<=8192 THEN latest_summary->'decisions' ELSE '[]'::jsonb END AS decisions \
         FROM conversations WHERE user_context_id=$1 AND agent_external_key=$2 AND summary_version>0 \
         ORDER BY updated_at DESC,id DESC LIMIT 50")
        .bind(owner.user_context_id.0).bind(agent_key).fetch_all(&mut *tx).await?;
    tx.commit().await?;
    let mut projection = AgentMemoryProjection {
        current_time_utc: chrono::Utc::now().to_rfc3339(),
        facts: profile,
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

fn bounded_json(projection: &mut AgentMemoryProjection) -> Result<String, sqlx::Error> {
    loop {
        let serialized = serde_json::to_string(projection)
            .map_err(|error| sqlx::Error::Decode(Box::new(error)))?;
        if serialized.len() <= MAX_CONTEXT_BYTES {
            return Ok(serialized);
        }
        if projection.recent_recaps.pop().is_some()
            || projection.decisions.pop().is_some()
            || projection.commitments.pop().is_some()
        {
            continue;
        }
        return Err(sqlx::Error::Protocol(
            "agent memory exceeds context budget".into(),
        ));
    }
}
