use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SpaceState {
    #[default]
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum NodeState {
    Running,
    #[default]
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
    #[serde(default)]
    pub title: String,
    pub mission: String,
    #[serde(default)]
    pub look_for: Vec<String>,
    pub done_when: String,
    #[serde(default)]
    pub limits: AgentSpecLimits,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    #[default]
    Idle,
    Running,
    Failed,
}

impl RunState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "idle" => Some(Self::Idle),
            "running" => Some(Self::Running),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SpaceMessage {
    pub id: Uuid,
    pub space_id: Uuid,
    pub role: String,
    pub text: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Space {
    pub id: Uuid,
    pub user_id: Uuid,
    pub title: String,
    pub intent: String,
    pub state: SpaceState,
    pub agent_spec: serde_json::Value,
    pub committed_collection_id: Option<Uuid>,
    pub run_state: RunState,
    pub run_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SpaceEdge {
    pub id: Uuid,
    pub space_id: Uuid,
    pub from_node: Uuid,
    pub to_node: Uuid,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct SpaceGraph {
    pub space: Space,
    pub nodes: Vec<SpaceNode>,
    pub edges: Vec<SpaceEdge>,
}
