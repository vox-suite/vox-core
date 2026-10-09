use crate::{application::pulse::service::PulseService, db::Db, domain::{identity::Actor, pulse::PulseDefinition}, identity::UserId, storage::pulse::PulseRepository};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum DataQueryToolError {
    #[error("{0}")]
    Query(#[from] crate::application::pulse::service::PulseError),
    #[error("Database not configured")]
    NotConfigured,
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FindSchemasArgs { pub query: String }
#[derive(Clone)]
pub struct FindSchemas { db: Option<Db>, user_id: UserId }
impl FindSchemas { pub fn new(db: Option<Db>, user_id: UserId) -> Self { Self { db, user_id } } }
impl Tool for FindSchemas {
    const NAME: &'static str = "find_schemas";
    type Args = FindSchemasArgs;
    type Output = Value;
    type Error = DataQueryToolError;
    fn description(&self) -> String { "Discover published measurements from the authenticated user's canonical timeline. Use the returned measurement_id, units and supported grouping in query_user_data. Missing coverage is unknown; separate currencies must remain separate.".into() }
    fn parameters(&self) -> Value { json!({"type":"object","additionalProperties":false,"required":["query"],"properties":{"query":{"type":"string","maxLength":200}}}) }
    async fn call(&self, _: &mut rig::prelude::ToolContext, args: Self::Args) -> Result<Value,Self::Error> {
        let db = self.db.as_ref().ok_or(DataQueryToolError::NotConfigured)?;
        let service = PulseService::new(PulseRepository::new(db.pool().clone()));
        let query = args.query.to_lowercase();
        let catalog = service.measurements(&Actor::user(self.user_id.0),"UTC").await?;
        Ok(json!({"measurements":catalog.into_iter().filter(|m| format!("{} {} {}",m.title,m.description,m.profile.category).to_lowercase().contains(&query)).collect::<Vec<_>>() }))
    }
}
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct QueryUserDataArgs { pub definition: PulseDefinition }
#[derive(Clone)]
pub struct QueryUserData { db: Option<Db>, user_id: UserId }
impl QueryUserData { pub fn new(db: Option<Db>, user_id: UserId) -> Self { Self { db,user_id } } }
impl Tool for QueryUserData {
    const NAME: &'static str = "query_user_data";
    type Args = QueryUserDataArgs;
    type Output = Value;
    type Error = DataQueryToolError;
    fn description(&self) -> String { "Compute an authorized published timeline measurement using Pulse. Discover its measurement_id first with find_schemas. SQL performs the arithmetic. A version 2 definition requires measurement_id, period_days, timezone, chart_type and exactly one bucket or dimension. Maximum range including offset is 366 days. Missing facts never mean zero.".into() }
    fn parameters(&self) -> Value { json!({"type":"object","required":["definition"],"additionalProperties":false,"properties":{"definition":{"type":"object","additionalProperties":false,"required":["version","measurement_id","period_days","timezone","chart_type"],"properties":{"version":{"type":"integer","enum":[2]},"measurement_id":{"type":"string"},"period_days":{"type":"integer","minimum":1,"maximum":366},"offset_days":{"type":"integer","minimum":0,"maximum":365},"timezone":{"type":"string"},"chart_type":{"type":"string","enum":["bar","line","area","pie","stat"]},"bucket":{"type":["string","null"],"enum":["day","week","month",null]},"dimension":{"type":["string","null"]}}}}}) }
    async fn call(&self, _: &mut rig::prelude::ToolContext, args: Self::Args) -> Result<Value,Self::Error> {
        let db = self.db.as_ref().ok_or(DataQueryToolError::NotConfigured)?;
        let service = PulseService::new(PulseRepository::new(db.pool().clone()));
        Ok(json!(service.preview(&Actor::user(self.user_id.0),args.definition).await?))
    }
}
