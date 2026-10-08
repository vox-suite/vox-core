use crate::{
    application::pulse::measurements::measurement_catalog,
    db::Db,
    domain::pulse_goals::{GoalDirection, GoalKind, GoalPeriod, GoalSuggestion},
    identity::{ResourceOwner, UserId},
    realtime::UserEventHub,
    storage::{pulse::PulseRepository, spaces::SpaceRepository},
};
use chrono::NaiveDate;
use rig::tool::Tool;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::space_graph::SpaceGraphToolError;

#[derive(Debug, Default, Deserialize)]
pub struct NoArgs {}

/// Lets the agent see which measurements a data-driven goal can track.
#[derive(Clone)]
pub struct ListGoalMeasurements {
    db: Option<Db>,
    user_id: UserId,
    owner: Option<ResourceOwner>,
}
impl ListGoalMeasurements {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self {
            db,
            user_id,
            owner: None,
        }
    }
    pub fn with_owner(mut self, owner: ResourceOwner) -> Self {
        self.user_id = owner.user_id;
        self.owner = Some(owner);
        self
    }
}
impl Tool for ListGoalMeasurements {
    const NAME: &'static str = "list_goal_measurements";
    type Args = NoArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "List the measurements from the user's own data that a goal can be tracked against (for example hours played, spending, listening). Use the returned id as measurement_id in propose_goal.".to_owned()
    }
    fn parameters(&self) -> Value {
        json!({ "type": "object", "properties": {} })
    }
    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        _args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        verify_native_goal_owner(db, self.user_id, self.owner).await?;
        let profiles = PulseRepository::new(db.pool().clone())
            .profiles(self.user_id.0)
            .await?;
        let catalog = crate::application::pulse::service::PulseService::new(PulseRepository::new(
            db.pool().clone(),
        ))
        .catalog_for_agent(self.user_id.0, measurement_catalog(&profiles), "general")
        .await
        .map_err(|e| SpaceGraphToolError::InvalidInput(e.to_string()))?;
        let list: Vec<Value> = catalog.into_iter()
            .take(60)
            .map(|m| json!({ "id": m.id, "title": m.title, "unit": m.unit, "description": m.description }))
            .collect();
        Ok(json!({ "measurements": list }))
    }
}

#[derive(Debug, Deserialize)]
pub struct ProposeGoalArgs {
    pub node_id: Uuid,
    pub title: String,
    pub kind: String,
    #[serde(default)]
    pub direction: Option<String>,
    #[serde(default)]
    pub period: Option<String>,
    pub target: f64,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub measurement_id: Option<String>,
    #[serde(default)]
    pub deadline: Option<String>,
}

/// Attaches a goal proposal to a node. It never creates the goal: the user approves it in the UI.
#[derive(Clone)]
pub struct ProposeGoal {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    user_events: Option<UserEventHub>,
    owner: Option<ResourceOwner>,
}
impl ProposeGoal {
    pub fn new(
        db: Option<Db>,
        user_id: UserId,
        space_id: Uuid,
        user_events: Option<UserEventHub>,
    ) -> Self {
        Self {
            db,
            user_id,
            space_id,
            user_events,
            owner: None,
        }
    }
    pub fn with_owner(mut self, owner: ResourceOwner) -> Self {
        self.user_id = owner.user_id;
        self.owner = Some(owner);
        self
    }
}
impl Tool for ProposeGoal {
    const NAME: &'static str = "propose_goal";
    type Args = ProposeGoalArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Propose a trackable goal on a plan or step node, such as saving an amount by a date or keeping spending under a limit. This only attaches a proposal; the user must approve it before any goal exists. Use kind 'saving' for money the user will put aside by hand (give unit, e.g. INR). Use kind 'metric' for something measured from the user's data (give measurement_id from list_goal_measurements; the target is in that measurement's unit). direction 'at_most' is a limit, 'at_least' is a target. period 'week' or 'month' makes it recurring; omit it for a one-off goal with a deadline (YYYY-MM-DD).".to_owned()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string" },
                "title": { "type": "string", "description": "2-6 plain words" },
                "kind": { "type": "string", "enum": ["saving", "metric"] },
                "direction": { "type": "string", "enum": ["at_least", "at_most"] },
                "period": { "type": "string", "enum": ["week", "month"] },
                "target": { "type": "number" },
                "unit": { "type": "string" },
                "measurement_id": { "type": "string" },
                "deadline": { "type": "string" }
            },
            "required": ["node_id", "title", "kind", "target"]
        })
    }
    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        verify_native_goal_owner(db, self.user_id, self.owner).await?;
        let invalid = |m: &str| SpaceGraphToolError::InvalidInput(m.into());
        let title: String = args.title.trim().chars().take(120).collect();
        if title.is_empty() {
            return Err(invalid("title is required"));
        }
        if !args.target.is_finite() || args.target <= 0.0 || args.target >= 1e12 {
            return Err(invalid("target must be a positive number"));
        }
        let kind: GoalKind = serde_json::from_value(json!(args.kind))
            .map_err(|_| invalid("kind must be 'saving' or 'metric'"))?;
        let direction: GoalDirection = match args.direction.as_deref() {
            Some(d) => serde_json::from_value(json!(d))
                .map_err(|_| invalid("direction must be 'at_least' or 'at_most'"))?,
            None => GoalDirection::AtLeast,
        };
        let period: Option<GoalPeriod> = match args.period.as_deref() {
            Some(p) => Some(
                serde_json::from_value(json!(p))
                    .map_err(|_| invalid("period must be 'week' or 'month'"))?,
            ),
            None => None,
        };
        let deadline = match args.deadline.as_deref() {
            Some(d) => Some(
                NaiveDate::parse_from_str(d, "%Y-%m-%d")
                    .map_err(|_| invalid("deadline must be YYYY-MM-DD"))?,
            ),
            None => None,
        };
        let mut unit = args
            .unit
            .map(|u| u.trim().to_owned())
            .filter(|u| !u.is_empty());
        let mut measurement_id = None;
        match kind {
            GoalKind::Saving => {
                if unit.is_none() {
                    return Err(invalid("a saving goal needs a unit such as INR"));
                }
            }
            GoalKind::Metric => {
                let id = args
                    .measurement_id
                    .ok_or_else(|| invalid("a metric goal needs a measurement_id"))?;
                let profiles = PulseRepository::new(db.pool().clone())
                    .profiles(self.user_id.0)
                    .await?;
                let catalog = crate::application::pulse::service::PulseService::new(
                    PulseRepository::new(db.pool().clone()),
                )
                .catalog_for_agent(self.user_id.0, measurement_catalog(&profiles), "general")
                .await
                .map_err(|e| invalid(&e.to_string()))?;
                let m = catalog.into_iter().find(|m| m.id == id).ok_or_else(|| {
                    invalid("unknown measurement_id; call list_goal_measurements first")
                })?;
                unit = Some(m.unit);
                measurement_id = Some(id);
            }
        }
        let repo = SpaceRepository::new(db.pool().clone());
        let node = repo
            .get_node(self.space_id, args.node_id)
            .await?
            .ok_or(SpaceGraphToolError::NotFound)?;
        if node.data.get("goal_id").is_some() {
            return Err(invalid("this node already has a goal"));
        }
        let proposal = GoalSuggestion {
            title,
            kind,
            direction,
            period,
            target: args.target,
            unit,
            measurement_id,
            deadline,
        };
        let mut data = node.data.clone();
        if !data.is_object() {
            data = json!({});
        }
        let mut stored =
            serde_json::to_value(&proposal).map_err(|_| invalid("could not store the proposal"))?;
        stored["status"] = json!("proposed");
        data["goal_proposal"] = stored;
        let updated = repo
            .update_node(
                self.space_id,
                node.id,
                None,
                None,
                Some(data),
                None,
                None,
                None,
            )
            .await?
            .ok_or(SpaceGraphToolError::NotFound)?;
        if let Some(hub) = &self.user_events {
            hub.notify(
                self.user_id.0,
                json!({ "type": "space_node_updated", "space_id": self.space_id, "node": updated }),
            );
        }
        Ok(json!({
            "node_id": updated.id,
            "status": "proposed",
            "note": "Waiting for the user to approve this goal on the node."
        }))
    }
}

async fn verify_native_goal_owner(
    db: &Db,
    user: UserId,
    owner: Option<ResourceOwner>,
) -> Result<(), SpaceGraphToolError> {
    let Some(owner) = owner else {
        return Err(SpaceGraphToolError::NotConfigured);
    };
    let valid: bool = sqlx::query_scalar(&format!(
        "SELECT COALESCE({}=$2,false)",
        crate::storage::pulse::NATIVE_CONTEXT
    ))
    .bind(user.0)
    .bind(owner.user_context_id.0)
    .fetch_one(db.pool())
    .await?;
    if !valid || owner.user_id != user {
        return Err(SpaceGraphToolError::NotConfigured);
    }
    Ok(())
}
