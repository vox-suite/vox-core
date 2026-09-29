use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SpaceState {
    Ideating,
    Planned,
    Committed,
    Dropped,
}

impl SpaceState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ideating => "ideating",
            Self::Planned => "planned",
            Self::Committed => "committed",
            Self::Dropped => "dropped",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "ideating" => Some(Self::Ideating),
            "planned" => Some(Self::Planned),
            "committed" => Some(Self::Committed),
            "dropped" => Some(Self::Dropped),
            _ => None,
        }
    }
}

impl Default for SpaceState {
    fn default() -> Self {
        Self::Ideating
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NodeState {
    Running,
    Done,
    Stale,
    Rejected,
}

impl NodeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Stale => "stale",
            Self::Rejected => "rejected",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "running" => Some(Self::Running),
            "done" => Some(Self::Done),
            "stale" => Some(Self::Stale),
            "rejected" => Some(Self::Rejected),
            _ => None,
        }
    }
}

impl Default for NodeState {
    fn default() -> Self {
        Self::Done
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSpecLimits {
    #[serde(default = "default_max_steps")]
    pub max_steps: usize,
    #[serde(default = "default_max_children")]
    pub max_children: usize,
}

fn default_max_steps() -> usize {
    crate::config::DEFAULT_SPACE_MAX_STEPS
}

fn default_max_children() -> usize {
    crate::config::DEFAULT_SPACE_MAX_CHILDREN
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AgentSpec {
    pub mission: String,
    #[serde(default)]
    pub look_for: Vec<String>,
    pub done_when: String,
    #[serde(default)]
    pub limits: AgentSpecLimits,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Space {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub intent: String,
    pub state: SpaceState,
    pub agent_spec: serde_json::Value,
    pub committed_collection_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceNode {
    pub id: Uuid,
    pub space_id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub data: serde_json::Value,
    pub state: NodeState,
    pub position: serde_json::Value,
    pub derived_from: Vec<Uuid>,
    pub provenance: serde_json::Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceEdge {
    pub id: Uuid,
    pub space_id: Uuid,
    pub from_node: Uuid,
    pub to_node: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceGraph {
    pub space: Space,
    pub nodes: Vec<SpaceNode>,
    pub edges: Vec<SpaceEdge>,
}
