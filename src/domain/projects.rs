use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectCitation {
    pub source_id: Uuid,
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContextVersion {
    pub title: String,
    pub body: String,
    pub citations: Vec<ProjectCitation>,
    pub updated_at: String,
    pub origin: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ContextNode {
    pub id: Uuid,
    pub kind: String,
    pub title: String,
    pub body: String,
    pub origin: String,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub citations: Vec<ProjectCitation>,
    #[serde(default)]
    pub history: Vec<ContextVersion>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectSource {
    pub id: Uuid,
    pub label: String,
    pub kind: String,
    pub files: usize,
    pub symbols: usize,
    pub fingerprint: String,
    pub indexed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectActivity {
    pub id: Uuid,
    pub text: String,
    pub at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectFlowNode {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub detail: String,
    #[serde(default)]
    pub citations: Vec<ProjectCitation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectFlowEdge {
    pub from: String,
    pub to: String,
    pub label: String,
    pub evidence: String,
    #[serde(default)]
    pub citations: Vec<ProjectCitation>,
}
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectFlow {
    pub title: String,
    pub nodes: Vec<ProjectFlowNode>,
    pub edges: Vec<ProjectFlowEdge>,
    #[serde(default)]
    pub gaps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectMessage {
    pub id: Uuid,
    pub role: String,
    pub body: String,
    pub at: String,
    #[serde(default)]
    pub proposals: Vec<ContextNode>,
    #[serde(default)]
    pub flow: Option<ProjectFlow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagram_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ProjectData {
    #[serde(default)]
    pub template: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub nodes: Vec<ContextNode>,
    #[serde(default)]
    pub sources: Vec<ProjectSource>,
    #[serde(default)]
    pub messages: Vec<ProjectMessage>,
    #[serde(default)]
    pub activity: Vec<ProjectActivity>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub data: ProjectData,
    pub revision: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ProjectData {
    pub fn validate(&self) -> bool {
        self.template.len() <= 40
            && self.notes.len() <= 12000
            && self.nodes.len() <= 300
            && self.sources.len() <= 30
            && self.messages.len() <= 100
            && self.activity.len() <= 200
            && self
                .nodes
                .iter()
                .chain(self.messages.iter().flat_map(|m| &m.proposals))
                .all(|n| {
                    matches!(
                        n.kind.as_str(),
                        "goal"
                            | "component"
                            | "decision"
                            | "constraint"
                            | "question"
                            | "asset"
                            | "milestone"
                    ) && matches!(n.origin.as_str(), "user" | "extracted" | "inferred")
                        && n.title.len() <= 300
                        && n.body.len() <= 12000
                        && n.history.len() <= 20
                        && n.citations.len() <= 20
                        && n.citations.iter().all(|c| {
                            !c.path.starts_with('/')
                                && !c.path.contains("..")
                                && c.path.len() <= 1000
                                && c.start_line > 0
                                && c.end_line >= c.start_line
                                && c.hash.len() <= 128
                        })
                })
            && self.messages.iter().all(|m| {
                matches!(m.role.as_str(), "user" | "assistant")
                    && m.body.len() <= 20000
                    && m.proposals.len() <= 12
                    && m.flow.as_ref().is_none_or(|f| {
                        f.diagram.as_ref().is_none_or(|diagram| {
                            diagram.is_object()
                                && diagram
                                    .get("diagram_type")
                                    .and_then(|v| v.as_str())
                                    .is_some_and(|kind| {
                                        matches!(
                                            kind,
                                            "architecture"
                                                | "workflow"
                                                | "sequence"
                                                | "dataflow"
                                                | "lifecycle"
                                        )
                                    })
                                && serde_json::to_vec(diagram)
                                    .is_ok_and(|data| data.len() <= 100_000)
                        }) && f.title.len() <= 300
                            && f.nodes.len() <= 40
                            && f.edges.len() <= 80
                            && f.gaps.len() <= 20
                            && f.gaps.iter().all(|g| g.len() <= 1000)
                            && f.nodes.iter().all(|n| {
                                !n.id.is_empty()
                                    && n.id.len() <= 100
                                    && n.title.len() <= 300
                                    && n.kind.len() <= 80
                                    && n.detail.len() <= 2000
                                    && n.citations.len() <= 20
                            })
                            && f.nodes
                                .iter()
                                .map(|n| &n.id)
                                .collect::<std::collections::HashSet<_>>()
                                .len()
                                == f.nodes.len()
                            && f.edges.iter().all(|e| {
                                e.label.len() <= 300
                                    && matches!(e.evidence.as_str(), "observed" | "inferred")
                                    && e.citations.len() <= 20
                                    && f.nodes.iter().any(|n| n.id == e.from)
                                    && f.nodes.iter().any(|n| n.id == e.to)
                            })
                    })
            })
            && self
                .nodes
                .iter()
                .map(|n| n.id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.nodes.len()
            && self
                .sources
                .iter()
                .map(|n| n.id)
                .collect::<std::collections::HashSet<_>>()
                .len()
                == self.sources.len()
    }

    pub fn context(&self) -> String {
        self.nodes
            .iter()
            .filter(|n| !n.stale)
            .take(80)
            .map(|n| {
                format!(
                    "[{}] {} ({}, {}): {}\nSources: {}",
                    n.id,
                    n.title,
                    n.kind,
                    n.origin,
                    n.body.chars().take(1600).collect::<String>(),
                    n.citations
                        .iter()
                        .map(|c| format!("{}:{}-{}", c.path, c.start_line, c.end_line))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
            .chars()
            .take(32000)
            .collect()
    }
}
