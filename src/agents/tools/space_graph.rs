use crate::{
    db::Db, domain::spaces::NodeState, identity::UserId, realtime::UserEventHub,
    storage::spaces::SpaceRepository,
};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum SpaceGraphToolError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),
    #[error("Invalid input: {0}")]
    InvalidInput(String),
    #[error("Space node not found")]
    NotFound,
    #[error("Database not configured")]
    NotConfigured,
    #[error("Effort limit reached: {0}")]
    LimitReached(String),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AddNodeArgs {
    pub kind: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub position: Option<Value>,
    #[serde(default)]
    pub derived_from: Option<Vec<Uuid>>,
    #[serde(default)]
    pub provenance: Option<Value>,
    #[serde(default)]
    pub connect_from: Option<Uuid>,
}

#[derive(Clone)]
pub struct AddNode {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    user_events: Option<UserEventHub>,
}

impl AddNode {
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
        }
    }
}

impl Tool for AddNode {
    const NAME: &'static str = "add_node";
    type Args = AddNodeArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Add a new node to the space graph representing an idea, option, research finding, budget, decision, or step.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "kind": { "type": "string", "description": "e.g. goal, research, option, decision, step, budget, risk, data" },
                "title": { "type": "string", "description": "Short headline" },
                "body": { "type": "string", "description": "Detailed notes or analysis" },
                "data": { "type": "object", "description": "Arbitrary structured data" },
                "position": { "type": "object", "description": "{x: number, y: number}" },
                "derived_from": { "type": "array", "items": { "type": "string" }, "description": "UUIDs of source nodes" },
                "provenance": { "type": "object", "description": "Source user data or external query context" },
                "connect_from": { "type": "string", "description": "Optional UUID of parent node to link from" }
            },
            "required": ["kind", "title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        let repo = SpaceRepository::new(db.pool().clone());

        let mut derived = args.derived_from.unwrap_or_default();
        if let Some(cf) = args.connect_from
            && !derived.contains(&cf)
        {
            derived.push(cf);
        }

        let node = repo
            .add_node(
                self.space_id,
                &args.kind,
                &args.title,
                args.body.as_deref().unwrap_or(""),
                args.data.unwrap_or_else(|| json!({})),
                NodeState::Done,
                args.position.unwrap_or_else(|| json!({"x": 0.0, "y": 0.0})),
                &derived,
                args.provenance.unwrap_or_else(|| json!({})),
            )
            .await?;

        if let Some(cf) = args.connect_from {
            let edge = repo.add_edge(self.space_id, cf, node.id).await?;
            if let Some(hub) = &self.user_events {
                hub.notify(
                    self.user_id.0,
                    json!({
                        "type": "space_edge_created",
                        "space_id": self.space_id,
                        "edge": edge
                    }),
                );
            }
        }

        if let Some(hub) = &self.user_events {
            hub.notify(
                self.user_id.0,
                json!({
                    "type": "space_node_created",
                    "space_id": self.space_id,
                    "node": node
                }),
            );
        }

        Ok(json!({
            "id": node.id,
            "title": node.title,
            "kind": node.kind,
            "state": node.state
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateNodeArgs {
    pub node_id: Uuid,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub position: Option<Value>,
    #[serde(default)]
    pub provenance: Option<Value>,
}

#[derive(Clone)]
pub struct UpdateNode {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    user_events: Option<UserEventHub>,
}

impl UpdateNode {
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
        }
    }
}

impl Tool for UpdateNode {
    const NAME: &'static str = "update_node";
    type Args = UpdateNodeArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Update an existing node's details, state, position, or findings in the space graph."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string" },
                "title": { "type": "string" },
                "body": { "type": "string" },
                "data": { "type": "object" },
                "state": { "type": "string", "enum": ["running", "done", "stale", "rejected"] },
                "position": { "type": "object" },
                "provenance": { "type": "object" }
            },
            "required": ["node_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        let repo = SpaceRepository::new(db.pool().clone());

        let parsed_state = args.state.as_deref().and_then(NodeState::parse);

        let updated = repo
            .update_node(
                self.space_id,
                args.node_id,
                args.title.as_deref(),
                args.body.as_deref(),
                args.data,
                parsed_state,
                args.position,
                args.provenance,
            )
            .await?;

        let Some(node) = updated else {
            return Err(SpaceGraphToolError::NotFound);
        };

        if parsed_state == Some(NodeState::Rejected) {
            let stale = repo.mark_descendants_stale(self.space_id, node.id).await?;
            if let Some(hub) = &self.user_events {
                for id in stale {
                    if let Ok(Some(n)) = repo.get_node(self.space_id, id).await {
                        hub.notify(
                            self.user_id.0,
                            json!({ "type": "space_node_updated", "space_id": self.space_id, "node": n }),
                        );
                    }
                }
            }
        }

        if let Some(hub) = &self.user_events {
            hub.notify(
                self.user_id.0,
                json!({
                    "type": "space_node_updated",
                    "space_id": self.space_id,
                    "node": node
                }),
            );
        }

        Ok(json!({
            "id": node.id,
            "title": node.title,
            "state": node.state
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct AddEdgeArgs {
    pub from_node: Uuid,
    pub to_node: Uuid,
}

#[derive(Clone)]
pub struct AddEdge {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    user_events: Option<UserEventHub>,
}

impl AddEdge {
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
        }
    }
}

impl Tool for AddEdge {
    const NAME: &'static str = "add_edge";
    type Args = AddEdgeArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Connect two nodes in the space graph to indicate relationship, dependency, or flow."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "from_node": { "type": "string" },
                "to_node": { "type": "string" }
            },
            "required": ["from_node", "to_node"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        let repo = SpaceRepository::new(db.pool().clone());

        let edge = repo
            .add_edge(self.space_id, args.from_node, args.to_node)
            .await?;

        if let Some(hub) = &self.user_events {
            hub.notify(
                self.user_id.0,
                json!({
                    "type": "space_edge_created",
                    "space_id": self.space_id,
                    "edge": edge
                }),
            );
        }

        Ok(json!({
            "id": edge.id,
            "from_node": edge.from_node,
            "to_node": edge.to_node
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RemoveNodeArgs {
    pub node_id: Uuid,
}

#[derive(Clone)]
pub struct RemoveNode {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    user_events: Option<UserEventHub>,
}

impl RemoveNode {
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
        }
    }
}

impl Tool for RemoveNode {
    const NAME: &'static str = "remove_node";
    type Args = RemoveNodeArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Remove a node and its incoming/outgoing edges from the space graph.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "node_id": { "type": "string" }
            },
            "required": ["node_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        let repo = SpaceRepository::new(db.pool().clone());

        let stale = repo
            .mark_descendants_stale(self.space_id, args.node_id)
            .await?;
        let deleted = repo.remove_node(self.space_id, args.node_id).await?;
        if deleted {
            if let Some(hub) = &self.user_events {
                for id in stale {
                    if let Ok(Some(n)) = repo.get_node(self.space_id, id).await {
                        hub.notify(
                            self.user_id.0,
                            json!({ "type": "space_node_updated", "space_id": self.space_id, "node": n }),
                        );
                    }
                }
            }
            if let Some(hub) = &self.user_events {
                hub.notify(
                    self.user_id.0,
                    json!({
                        "type": "space_node_deleted",
                        "space_id": self.space_id,
                        "node_id": args.node_id
                    }),
                );
            }
        }

        Ok(json!({ "deleted": deleted }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SpawnBranchArgs {
    pub parent_node_id: Uuid,
    pub title: String,
    pub brief: String,
}

#[derive(Clone)]
pub struct SpawnBranch {
    db: Option<Db>,
    user_id: UserId,
    space_id: Uuid,
    max_children: usize,
    user_events: Option<UserEventHub>,
}

impl SpawnBranch {
    pub fn new(
        db: Option<Db>,
        user_id: UserId,
        space_id: Uuid,
        max_children: usize,
        user_events: Option<UserEventHub>,
    ) -> Self {
        Self {
            db,
            user_id,
            space_id,
            max_children,
            user_events,
        }
    }
}

impl Tool for SpawnBranch {
    const NAME: &'static str = "spawn_branch";
    type Args = SpawnBranchArgs;
    type Output = Value;
    type Error = SpaceGraphToolError;

    fn description(&self) -> String {
        "Spawn an exploratory research branch under a parent node in the space graph.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "parent_node_id": { "type": "string" },
                "title": { "type": "string", "description": "Candidate or exploration name" },
                "brief": { "type": "string", "description": "Specific focus of this branch" }
            },
            "required": ["parent_node_id", "title", "brief"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(SpaceGraphToolError::NotConfigured)?;
        let repo = SpaceRepository::new(db.pool().clone());

        let existing_nodes = repo.list_nodes(self.space_id).await?;
        let child_count = existing_nodes
            .iter()
            .filter(|n| n.derived_from.contains(&args.parent_node_id))
            .count();

        if child_count >= self.max_children {
            let limit_node = repo
                .add_node(
                    self.space_id,
                    "limit",
                    "Limit reached",
                    &format!(
                        "Max parallel branches ({}) reached for this stage.",
                        self.max_children
                    ),
                    json!({ "max_children": self.max_children }),
                    NodeState::Done,
                    json!({"x": 0.0, "y": 0.0}),
                    &[args.parent_node_id],
                    json!({}),
                )
                .await?;
            let _ = repo
                .add_edge(self.space_id, args.parent_node_id, limit_node.id)
                .await;

            return Err(SpaceGraphToolError::LimitReached(format!(
                "Max children limit ({}) reached",
                self.max_children
            )));
        }

        let node = repo
            .add_node(
                self.space_id,
                "research",
                &args.title,
                &args.brief,
                json!({ "brief": args.brief }),
                NodeState::Running,
                json!({"x": 0.0, "y": 0.0}),
                &[args.parent_node_id],
                json!({}),
            )
            .await?;

        let edge = repo
            .add_edge(self.space_id, args.parent_node_id, node.id)
            .await?;

        if let Some(hub) = &self.user_events {
            hub.notify(
                self.user_id.0,
                json!({
                    "type": "space_node_created",
                    "space_id": self.space_id,
                    "node": node
                }),
            );
            hub.notify(
                self.user_id.0,
                json!({
                    "type": "space_edge_created",
                    "space_id": self.space_id,
                    "edge": edge
                }),
            );
        }

        Ok(json!({
            "id": node.id,
            "title": node.title,
            "status": "spawned"
        }))
    }
}
