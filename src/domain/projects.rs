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
pub struct ProjectMessage {
    pub id: Uuid,
    pub role: String,
    pub body: String,
    pub at: String,
    #[serde(default)]
    pub proposals: Vec<ContextNode>,
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
