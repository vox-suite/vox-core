use crate::{db::Db, domain::spans::SpanQuery, identity::UserId, storage::spans::SpanRepository};
use chrono::{DateTime, Utc};
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum VisitsToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Database not configured")]
    NotConfigured,
}

#[derive(Debug, Deserialize)]
pub struct ListVisitsArgs {
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListVisits {
    db: Option<Db>,
    user_id: UserId,
}

impl ListVisits {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListVisits {
    const NAME: &'static str = "list_visits";
    type Args = ListVisitsArgs;
    type Output = Value;
    type Error = VisitsToolError;

    fn description(&self) -> String {
        "List places the user visited (name, coordinates, time) from their location history. Use the coordinates with show_on_map.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from": { "type": "string", "description": "RFC 3339 start of range" },
                "to": { "type": "string", "description": "RFC 3339 end of range" },
                "limit": { "type": "integer", "description": "Max visits, default 30, max 50" }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(VisitsToolError::NotConfigured)?;
        let repo = SpanRepository::new(db.pool().clone());
        let spans = repo
            .list(
                self.user_id.0,
                &SpanQuery {
                    from: args.from,
                    to: args.to,
                    limit: Some(500),
                    ..Default::default()
                },
            )
            .await?;
        let visits: Vec<Value> = spans
            .into_iter()
            .filter(|s| s.source == "location" && s.category == "visit")
            .filter_map(|s| {
                let lat = s.data.get("lat")?.as_f64()?;
                let lng = s.data.get("lng")?.as_f64()?;
                Some(json!({
                    "id": s.id,
                    "place": s.title,
                    "lat": lat,
                    "lng": lng,
                    "startAt": s.start_at,
                    "endAt": s.end_at,
                }))
            })
            .take(args.limit.unwrap_or(30).clamp(1, 50) as usize)
            .collect();
        Ok(json!({ "visits": visits }))
    }
}
