use crate::{
    db::Db,
    domain::timeline::{IngestTimelineEventInput, NewEventType},
    identity::UserId,
    storage::timeline::{TimelineRepository, TimelineStorageError},
};
use rig::tool::Tool;
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum TimelineToolError {
    #[error("Database not configured")]
    NotConfigured,
    #[error(transparent)]
    Storage(#[from] TimelineStorageError),
}
#[derive(Clone)]
pub struct CreateTimelineEventType {
    pub db: Option<Db>,
    pub user_id: UserId,
}
impl Tool for CreateTimelineEventType {
    const NAME: &'static str = "create_timeline_event_type";
    type Args = NewEventType;
    type Output = Value;
    type Error = TimelineToolError;
    fn description(&self) -> String {
        "Publish a new versioned event type for this user when existing timeline types cannot represent a factual event. Use a fixed timeline group UUID returned by list_timeline_types. Provide a bounded JSON Schema with local references only. Numeric analytics metrics declare field, aggregation, unit and title; never create a separate financial table.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"required":["value","label","group_id","content_schema"],"properties":{"value":{"type":"string"},"label":{"type":"string"},"group_id":{"type":"string","format":"uuid"},"description":{"type":"string"},"content_schema":{"type":"object"},"analytics_definition":{"type":"object"},"ui_hint":{"type":"object"}}})
    }
    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Value, Self::Error> {
        let db = self.db.as_ref().ok_or(TimelineToolError::NotConfigured)?;
        Ok(json!(
            TimelineRepository::new(db.pool().clone())
                .create_event_type(self.user_id.0, args)
                .await?
        ))
    }
}
#[derive(Clone)]
pub struct ListTimelineTypes {
    pub db: Option<Db>,
    pub user_id: UserId,
}
impl Tool for ListTimelineTypes {
    const NAME: &'static str = "list_timeline_types";
    type Args = Value;
    type Output = Value;
    type Error = TimelineToolError;
    fn description(&self) -> String {
        "List fixed timeline groups and published event types visible to this user, including content schemas and analytics definitions.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"properties":{}})
    }
    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        _: Value,
    ) -> Result<Value, Self::Error> {
        let db = self.db.as_ref().ok_or(TimelineToolError::NotConfigured)?;
        let repo = TimelineRepository::new(db.pool().clone());
        Ok(
            json!({"groups":repo.list_groups().await?,"event_types":repo.list_event_types(self.user_id.0,None,None).await?}),
        )
    }
}
#[derive(Clone)]
pub struct SaveTimelineEvent {
    pub db: Option<Db>,
    pub user_id: UserId,
}
impl Tool for SaveTimelineEvent {
    const NAME: &'static str = "save_timeline_event";
    type Args = IngestTimelineEventInput;
    type Output = Value;
    type Error = TimelineToolError;
    fn description(&self) -> String {
        "Save a factual canonical timeline event for the current user. Use a published event type and validate content against its schema. Supply actual occurred_at and precision; a booking is not a visit, and a cumulative observation is not a session. Never invent dates, durations, currency, or financial facts. Attach source evidence and a stable dedupe_key.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"required":["title","occurred_at","content"],"properties":{"event_type_id":{"type":["string","null"]},"event_type_value":{"type":["string","null"]},"group_id":{"type":["string","null"]},"group_value":{"type":["string","null"]},"title":{"type":"string"},"summary":{"type":["string","null"]},"occurred_at":{"type":"string","format":"date-time"},"ended_at":{"type":["string","null"]},"time_precision":{"type":"string"},"source_timezone":{"type":["string","null"]},"content":{"type":"object"},"confidence":{"type":"number","minimum":0,"maximum":1},"dedupe_key":{"type":["string","null"]},"evidence":{"type":"array","items":{"type":"object"}}}})
    }
    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Value, Self::Error> {
        let db = self.db.as_ref().ok_or(TimelineToolError::NotConfigured)?;
        Ok(json!(
            TimelineRepository::new(db.pool().clone())
                .ingest_event(self.user_id.0, args)
                .await?
        ))
    }
}

#[derive(Clone)]
pub struct QueryTimelineEvents {
    pub db: Option<Db>,
    pub user_id: UserId,
}
impl Tool for QueryTimelineEvents {
    const NAME: &'static str = "query_timeline_events";
    type Args = crate::domain::timeline::TimelineQuery;
    type Output = Value;
    type Error = TimelineToolError;
    fn description(&self) -> String {
        "Read factual timeline entries for this user in an explicit date window, with at most 20 entries per page. Use this for event details and provenance. Use find_schemas and query_user_data for arithmetic or aggregate statistics. Dates with day precision do not establish a precise time.".into()
    }
    fn parameters(&self) -> Value {
        json!({"type":"object","additionalProperties":false,"required":["start_at","end_at"],"properties":{"start_at":{"type":"string","format":"date-time"},"end_at":{"type":"string","format":"date-time"},"group_value":{"type":"string"},"event_type_value":{"type":"string"},"cursor":{"type":"string"},"limit":{"type":"integer","minimum":1,"maximum":20}}})
    }
    async fn call(
        &self,
        _: &mut rig::prelude::ToolContext,
        mut args: Self::Args,
    ) -> Result<Value, Self::Error> {
        let db = self.db.as_ref().ok_or(TimelineToolError::NotConfigured)?;
        if args
            .start_at
            .zip(args.end_at)
            .is_none_or(|(start, end)| end <= start || end - start > chrono::Duration::days(366))
        {
            return Err(TimelineStorageError::ValidationFailed(
                "Choose a date window of at most 366 days".into(),
            )
            .into());
        }
        args.limit = Some(args.limit.unwrap_or(20).clamp(1, 20));
        let page = TimelineRepository::new(db.pool().clone())
            .query_events(self.user_id.0, args)
            .await?;
        let result = json!({"events":page.events.into_iter().map(|entry|json!({"event":entry.event,"sources":entry.evidence.into_iter().map(|evidence|json!({"source_type":evidence.source_type,"source_id":evidence.source_id,"source_record_id":evidence.source_record_id})).collect::<Vec<_>>()})).collect::<Vec<_>>(),"next_cursor":page.next_cursor});
        if serde_json::to_vec(&result).map_or(true, |bytes| bytes.len() > 262144) {
            return Err(TimelineStorageError::ValidationFailed(
                "Event details exceed the response limit; request fewer entries".into(),
            )
            .into());
        }
        Ok(result)
    }
}
