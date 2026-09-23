/**
* Agent tools for managing user projects, notes, and tasks.
*/
use crate::{db::Db, identity::UserId};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};
use uuid::Uuid;

#[derive(Debug)]
pub enum ProjectToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotFound(String),
    NotConfigured,
}

impl fmt::Display for ProjectToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for ProjectToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for ProjectToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateProjectArgs {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone)]
pub struct CreateProject {
    db: Option<Db>,
    user_id: UserId,
}

impl CreateProject {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for CreateProject {
    const NAME: &'static str = "create_project";
    type Args = CreateProjectArgs;
    type Output = Value;
    type Error = ProjectToolError;

    fn description(&self) -> String {
        "Create a new project to group and organize tasks, goals, and plans.".to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "The title or name of the project"
                },
                "description": {
                    "type": "string",
                    "description": "Optional description or objective of the project"
                }
            },
            "required": ["name"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            name = %args.name,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProjectToolError::NotConfigured)?;
        let name = args.name.trim();
        if name.is_empty() {
            return Err(ProjectToolError::InvalidInput(
                "Project name cannot be empty".into(),
            ));
        }
        let desc = args.description.as_deref().unwrap_or("").trim();

        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO collections (user_id, name, description, status, kind) \
             VALUES ($1, $2, $3, 'active', 'project') \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(name)
        .bind(desc)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "created",
            "project_id": id.to_string(),
            "name": name,
            "description": desc
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListProjectsArgs {
    pub status: Option<String>,
}

#[derive(Clone)]
pub struct ListProjects {
    db: Option<Db>,
    user_id: UserId,
}

impl ListProjects {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListProjects {
    const NAME: &'static str = "list_projects";
    type Args = ListProjectsArgs;
    type Output = Value;
    type Error = ProjectToolError;

    fn description(&self) -> String {
        "List existing projects for the user. Optionally filter by status (e.g. 'active', 'completed', 'paused')."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["active", "paused", "completed", "archived", "all"],
                    "description": "Filter by status; defaults to active"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            status = ?args.status,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProjectToolError::NotConfigured)?;
        let status_filter = args.status.as_deref().unwrap_or("active");

        let rows = if status_filter == "all" {
            sqlx::query(
                "SELECT p.id, p.name, p.description, p.status, p.created_at, \
                 COUNT(t.id) as task_count \
                 FROM collections p \
                 LEFT JOIN tasks t ON t.collection_id = p.id \
                 WHERE p.user_id = $1 AND p.kind = 'project' \
                 GROUP BY p.id \
                 ORDER BY p.updated_at DESC",
            )
            .bind(self.user_id.0)
            .fetch_all(db.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT p.id, p.name, p.description, p.status, p.created_at, \
                 COUNT(t.id) as task_count \
                 FROM collections p \
                 LEFT JOIN tasks t ON t.collection_id = p.id \
                 WHERE p.user_id = $1 AND p.kind = 'project' AND p.status = $2 \
                 GROUP BY p.id \
                 ORDER BY p.updated_at DESC",
            )
            .bind(self.user_id.0)
            .bind(status_filter)
            .fetch_all(db.pool())
            .await?
        };

        let list: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let name: String = r.get("name");
                let desc: String = r.get("description");
                let status: String = r.get("status");
                let task_count: i64 = r.get("task_count");
                json!({
                    "id": id.to_string(),
                    "name": name,
                    "description": desc,
                    "status": status,
                    "task_count": task_count
                })
            })
            .collect();

        Ok(json!({ "projects": list }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetProjectArgs {
    pub project_id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone)]
pub struct GetProject {
    db: Option<Db>,
    user_id: UserId,
}

impl GetProject {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for GetProject {
    const NAME: &'static str = "get_project";
    type Args = GetProjectArgs;
    type Output = Value;
    type Error = ProjectToolError;

    fn description(&self) -> String {
        "Get detailed information about a specific project by ID or project name, including associated tasks."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "The UUID of the project"
                },
                "name": {
                    "type": "string",
                    "description": "The exact or partial name of the project"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            project_id = ?args.project_id,
            name = ?args.name,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProjectToolError::NotConfigured)?;
        let row = if let Some(pid_str) = args.project_id {
            let pid = Uuid::parse_str(pid_str.trim())
                .map_err(|_| ProjectToolError::InvalidInput("Invalid UUID format".into()))?;
            sqlx::query(
                "SELECT id, name, description, status, created_at, updated_at \
                 FROM collections WHERE id = $1 AND user_id = $2 AND kind = 'project'",
            )
            .bind(pid)
            .bind(self.user_id.0)
            .fetch_optional(db.pool())
            .await?
        } else if let Some(name) = args.name {
            sqlx::query(
                "SELECT id, name, description, status, created_at, updated_at \
                 FROM collections WHERE user_id = $1 AND kind = 'project' AND LOWER(name) = LOWER($2) LIMIT 1",
            )
            .bind(self.user_id.0)
            .bind(name.trim())
            .fetch_optional(db.pool())
            .await?
        } else {
            return Err(ProjectToolError::InvalidInput(
                "Must provide either project_id or name".into(),
            ));
        };

        let r = match row {
            Some(row) => row,
            None => return Err(ProjectToolError::NotFound("Project not found".into())),
        };

        let project_id: Uuid = r.get("id");
        let name: String = r.get("name");
        let description: String = r.get("description");
        let status: String = r.get("status");

        let task_rows = sqlx::query(
            "SELECT id, title, status, execution_type, due_at \
             FROM tasks WHERE collection_id = $1 ORDER BY created_at ASC",
        )
        .bind(project_id)
        .fetch_all(db.pool())
        .await?;

        let tasks: Vec<Value> = task_rows
            .into_iter()
            .map(|tr| {
                let tid: Uuid = tr.get("id");
                let title: String = tr.get("title");
                let t_status: String = tr.get("status");
                let exec_type: String = tr.get("execution_type");
                json!({
                    "id": tid.to_string(),
                    "title": title,
                    "status": t_status,
                    "execution_type": exec_type
                })
            })
            .collect();

        Ok(json!({
            "id": project_id.to_string(),
            "name": name,
            "description": description,
            "status": status,
            "tasks": tasks
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateProjectArgs {
    pub project_id: String,
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<String>,
}

#[derive(Clone)]
pub struct UpdateProject {
    db: Option<Db>,
    user_id: UserId,
}

impl UpdateProject {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for UpdateProject {
    const NAME: &'static str = "update_project";
    type Args = UpdateProjectArgs;
    type Output = Value;
    type Error = ProjectToolError;

    fn description(&self) -> String {
        "Update project details, or change project status to active, paused, completed, or archived."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "project_id": {
                    "type": "string",
                    "description": "The UUID of the project to update"
                },
                "name": {
                    "type": "string",
                    "description": "New project name"
                },
                "description": {
                    "type": "string",
                    "description": "New project description"
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "paused", "completed", "archived"],
                    "description": "New status for the project"
                }
            },
            "required": ["project_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            project_id = %args.project_id,
            status = ?args.status,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(ProjectToolError::NotConfigured)?;
        let pid = Uuid::parse_str(args.project_id.trim())
            .map_err(|_| ProjectToolError::InvalidInput("Invalid project UUID".into()))?;

        let res = sqlx::query(
            "UPDATE collections SET \
             name = COALESCE($1, name), \
             description = COALESCE($2, description), \
             status = COALESCE($3, status), \
             updated_at = now() \
             WHERE id = $4 AND user_id = $5 AND kind = 'project'",
        )
        .bind(args.name.as_deref().map(str::trim))
        .bind(args.description.as_deref().map(str::trim))
        .bind(args.status.as_deref().map(str::trim))
        .bind(pid)
        .bind(self.user_id.0)
        .execute(db.pool())
        .await?;

        if res.rows_affected() == 0 {
            return Err(ProjectToolError::NotFound(
                "Project not found or not owned by user".into(),
            ));
        }

        Ok(json!({
            "status": "updated",
            "project_id": pid.to_string()
        }))
    }
}
