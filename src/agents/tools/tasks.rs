/**
* Agent tools for creating, updating, and querying user tasks.
*/
use crate::{db::Db, identity::ResourceOwner};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};
use uuid::Uuid;

#[derive(Debug)]
pub enum TaskToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    NotFound(String),
    NotConfigured,
}

impl fmt::Display for TaskToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for TaskToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for TaskToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTaskArgs {
    pub title: String,
    pub instruction: Option<String>,
    pub project_name: Option<String>,
    pub project_id: Option<String>,
    pub execution_type: Option<String>,
    pub due_at: Option<String>,
}

#[derive(Clone)]
pub struct CreateTask {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl CreateTask {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for CreateTask {
    const NAME: &'static str = "create_task";
    type Args = CreateTaskArgs;
    type Output = Value;
    type Error = TaskToolError;

    fn description(&self) -> String {
        "Create a new task for the user. Can be automatically bound to a project by name or ID. Specify execution_type as autonomous, interactive, or manual_human."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Short title or summary of the task"
                },
                "instruction": {
                    "type": "string",
                    "description": "Detailed raw instruction or steps required to complete the task"
                },
                "project_name": {
                    "type": "string",
                    "description": "Optional name of project to bind to (will match existing or auto-create)"
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional UUID of an existing project"
                },
                "execution_type": {
                    "type": "string",
                    "enum": ["autonomous", "interactive", "manual_human"],
                    "description": "How the task will be executed; defaults to manual_human unless doable by agent"
                },
                "due_at": {
                    "type": "string",
                    "description": "Optional ISO 8601 timestamp for when the task is due"
                }
            },
            "required": ["title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.owner.user_id.0,
            user_context_id = %self.owner.user_context_id.0,
            title = %args.title,
            project = ?args.project_name,
            due_at = ?args.due_at,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(TaskToolError::NotConfigured)?;
        let title = args.title.trim();
        if title.is_empty() {
            return Err(TaskToolError::InvalidInput(
                "Task title cannot be empty".into(),
            ));
        }
        let raw_instruction = args.instruction.as_deref().unwrap_or(title).trim();
        let execution_type = args.execution_type.as_deref().unwrap_or("manual_human");

        let mut bound_project_id: Option<Uuid> = None;

        if let Some(pid_str) = args.project_id {
            let pid = Uuid::parse_str(pid_str.trim())
                .map_err(|_| TaskToolError::InvalidInput("Invalid project UUID".into()))?;
            let owned = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id = $1 AND user_id = $2)",
            )
            .bind(pid)
            .bind(self.owner.user_id.0)
            .fetch_one(db.pool())
            .await?;
            if !owned {
                return Err(TaskToolError::NotFound("Project not found".into()));
            }
            bound_project_id = Some(pid);
        } else if let Some(pname) = args.project_name {
            let pname_clean = pname.trim();
            if !pname_clean.is_empty() {
                let existing = sqlx::query_scalar::<_, Uuid>(
                    "SELECT id FROM projects WHERE user_id = $1 AND LOWER(name) = LOWER($2) LIMIT 1",
                )
                .bind(self.owner.user_id.0)
                .bind(pname_clean)
                .fetch_optional(db.pool())
                .await?;

                if let Some(pid) = existing {
                    bound_project_id = Some(pid);
                } else {
                    let new_pid = sqlx::query_scalar::<_, Uuid>(
                        "INSERT INTO projects (user_id, name, description, status) \
                         VALUES ($1, $2, 'Auto-created project', 'active') \
                         RETURNING id",
                    )
                    .bind(self.owner.user_id.0)
                    .bind(pname_clean)
                    .fetch_one(db.pool())
                    .await?;
                    bound_project_id = Some(new_pid);
                }
            }
        }

        let due_at_parsed: Option<chrono::DateTime<chrono::Utc>> =
            args.due_at.as_deref().and_then(|s| {
                chrono::DateTime::parse_from_rfc3339(s)
                    .ok()
                    .map(|dt| dt.with_timezone(&chrono::Utc))
                    .or_else(|| {
                        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
                            .ok()
                            .map(|naive| {
                                chrono::DateTime::<chrono::Utc>::from_naive_utc_and_offset(
                                    naive,
                                    chrono::Utc,
                                )
                            })
                    })
            });

        let task_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO tasks (user_context_id, user_id, project_id, title, raw_instruction, status, execution_type, due_at) \
             VALUES ($1, $2, $3, $4, $5, 'pending', $6, $7) \
             RETURNING id",
        )
        .bind(self.owner.user_context_id.0)
        .bind(self.owner.user_id.0)
        .bind(bound_project_id)
        .bind(title)
        .bind(raw_instruction)
        .bind(execution_type)
        .bind(due_at_parsed)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "created",
            "task_id": task_id.to_string(),
            "title": title,
            "project_id": bound_project_id.map(|id| id.to_string()),
            "execution_type": execution_type
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListTasksArgs {
    pub status: Option<String>,
    pub project_id: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListTasks {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl ListTasks {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for ListTasks {
    const NAME: &'static str = "list_tasks";
    type Args = ListTasksArgs;
    type Output = Value;
    type Error = TaskToolError;

    fn description(&self) -> String {
        "List tasks for the user. Can filter by status (pending, executing, completed, cancelled, all) or project_id."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "status": {
                    "type": "string",
                    "enum": ["pending", "evaluating", "executing", "waiting_user", "completed", "failed", "cancelled", "all"],
                    "description": "Task status filter; defaults to pending"
                },
                "project_id": {
                    "type": "string",
                    "description": "Optional project UUID to filter tasks for a specific project"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of tasks to return; defaults to 20"
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
            user_id = %self.owner.user_id.0,
            user_context_id = %self.owner.user_context_id.0,
            status = ?args.status,
            project_id = ?args.project_id,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(TaskToolError::NotConfigured)?;
        let status_filter = args.status.as_deref().unwrap_or("pending");
        let limit = args.limit.unwrap_or(20).clamp(1, 100);

        let rows = if let Some(pid_str) = args.project_id {
            let pid = Uuid::parse_str(pid_str.trim())
                .map_err(|_| TaskToolError::InvalidInput("Invalid project UUID".into()))?;

            if status_filter == "all" {
                sqlx::query(
                    "SELECT t.id, t.title, t.status, t.execution_type, t.due_at, t.created_at, p.name as project_name \
                     FROM tasks t \
                     LEFT JOIN projects p ON p.id = t.project_id \
                     WHERE t.user_id = $1 AND (t.user_context_id = $2 OR t.user_context_id IS NULL) \
                       AND t.project_id = $3 \
                     ORDER BY t.created_at DESC LIMIT $4",
                )
                .bind(self.owner.user_id.0)
                .bind(self.owner.user_context_id.0)
                .bind(pid)
                .bind(limit)
                .fetch_all(db.pool())
                .await?
            } else {
                sqlx::query(
                    "SELECT t.id, t.title, t.status, t.execution_type, t.due_at, t.created_at, p.name as project_name \
                     FROM tasks t \
                     LEFT JOIN projects p ON p.id = t.project_id \
                     WHERE t.user_id = $1 AND (t.user_context_id = $2 OR t.user_context_id IS NULL) \
                       AND t.project_id = $3 AND t.status = $4 \
                     ORDER BY t.created_at DESC LIMIT $5",
                )
                .bind(self.owner.user_id.0)
                .bind(self.owner.user_context_id.0)
                .bind(pid)
                .bind(status_filter)
                .bind(limit)
                .fetch_all(db.pool())
                .await?
            }
        } else if status_filter == "all" {
            sqlx::query(
                "SELECT t.id, t.title, t.status, t.execution_type, t.due_at, t.created_at, p.name as project_name \
                 FROM tasks t \
                 LEFT JOIN projects p ON p.id = t.project_id \
                 WHERE t.user_id = $1 AND (t.user_context_id = $2 OR t.user_context_id IS NULL) \
                 ORDER BY t.created_at DESC LIMIT $3",
            )
            .bind(self.owner.user_id.0)
            .bind(self.owner.user_context_id.0)
            .bind(limit)
            .fetch_all(db.pool())
            .await?
        } else {
            sqlx::query(
                "SELECT t.id, t.title, t.status, t.execution_type, t.due_at, t.created_at, p.name as project_name \
                 FROM tasks t \
                 LEFT JOIN projects p ON p.id = t.project_id \
                 WHERE t.user_id = $1 AND (t.user_context_id = $2 OR t.user_context_id IS NULL) \
                   AND t.status = $3 \
                 ORDER BY t.created_at DESC LIMIT $4",
            )
            .bind(self.owner.user_id.0)
            .bind(self.owner.user_context_id.0)
            .bind(status_filter)
            .bind(limit)
            .fetch_all(db.pool())
            .await?
        };

        let list: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let title: String = r.get("title");
                let status: String = r.get("status");
                let exec_type: String = r.get("execution_type");
                let project_name: Option<String> = r.get("project_name");
                json!({
                    "id": id.to_string(),
                    "title": title,
                    "status": status,
                    "execution_type": exec_type,
                    "project_name": project_name
                })
            })
            .collect();

        Ok(json!({ "tasks": list }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct GetTaskArgs {
    pub task_id: Option<String>,
    pub title_query: Option<String>,
}

#[derive(Clone)]
pub struct GetTask {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl GetTask {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for GetTask {
    const NAME: &'static str = "get_task";
    type Args = GetTaskArgs;
    type Output = Value;
    type Error = TaskToolError;

    fn description(&self) -> String {
        "Get detailed information about a specific task, its instruction, status, and execution result."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The UUID of the task"
                },
                "title_query": {
                    "type": "string",
                    "description": "Search keyword matching the task title"
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
            user_id = %self.owner.user_id.0,
            user_context_id = %self.owner.user_context_id.0,
            task_id = ?args.task_id,
            title_query = ?args.title_query,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(TaskToolError::NotConfigured)?;
        let row = if let Some(tid_str) = args.task_id {
            let tid = Uuid::parse_str(tid_str.trim())
                .map_err(|_| TaskToolError::InvalidInput("Invalid task UUID".into()))?;
            sqlx::query(
                "SELECT t.id, t.title, t.raw_instruction, t.status, t.execution_type, \
                 t.feasibility_reasoning, t.execution_result, t.due_at, t.created_at, t.completed_at, \
                 p.name as project_name \
                 FROM tasks t \
                 LEFT JOIN projects p ON p.id = t.project_id \
                 WHERE t.id = $1 AND t.user_id = $2 \
                   AND (t.user_context_id = $3 OR t.user_context_id IS NULL)",
            )
            .bind(tid)
            .bind(self.owner.user_id.0)
            .bind(self.owner.user_context_id.0)
            .fetch_optional(db.pool())
            .await?
        } else if let Some(q) = args.title_query {
            sqlx::query(
                "SELECT t.id, t.title, t.raw_instruction, t.status, t.execution_type, \
                 t.feasibility_reasoning, t.execution_result, t.due_at, t.created_at, t.completed_at, \
                 p.name as project_name \
                 FROM tasks t \
                 LEFT JOIN projects p ON p.id = t.project_id \
                 WHERE t.user_id = $1 AND (t.user_context_id = $2 OR t.user_context_id IS NULL) \
                   AND t.title ILIKE '%' || $3 || '%' \
                 ORDER BY t.created_at DESC LIMIT 1",
            )
            .bind(self.owner.user_id.0)
            .bind(self.owner.user_context_id.0)
            .bind(q.trim())
            .fetch_optional(db.pool())
            .await?
        } else {
            return Err(TaskToolError::InvalidInput(
                "Must provide either task_id or title_query".into(),
            ));
        };

        let r = match row {
            Some(row) => row,
            None => return Err(TaskToolError::NotFound("Task not found".into())),
        };

        let id: Uuid = r.get("id");
        let title: String = r.get("title");
        let instruction: String = r.get("raw_instruction");
        let status: String = r.get("status");
        let exec_type: String = r.get("execution_type");
        let reasoning: Option<String> = r.get("feasibility_reasoning");
        let result: Value = r.get("execution_result");
        let project_name: Option<String> = r.get("project_name");

        Ok(json!({
            "id": id.to_string(),
            "title": title,
            "instruction": instruction,
            "status": status,
            "execution_type": exec_type,
            "feasibility_reasoning": reasoning,
            "execution_result": result,
            "project_name": project_name
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateTaskArgs {
    pub task_id: String,
    pub status: Option<String>,
    pub feasibility_reasoning: Option<String>,
    pub execution_result: Option<Value>,
}

#[derive(Clone)]
pub struct UpdateTask {
    db: Option<Db>,
    owner: ResourceOwner,
}

impl UpdateTask {
    pub fn new(db: Option<Db>, owner: ResourceOwner) -> Self {
        Self { db, owner }
    }
}

impl Tool for UpdateTask {
    const NAME: &'static str = "update_task";
    type Args = UpdateTaskArgs;
    type Output = Value;
    type Error = TaskToolError;

    fn description(&self) -> String {
        "Update task status (e.g. mark 'completed', 'executing', 'failed', 'cancelled'), or record execution results."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "task_id": {
                    "type": "string",
                    "description": "The UUID of the task to update"
                },
                "status": {
                    "type": "string",
                    "enum": ["pending", "evaluating", "executing", "waiting_user", "completed", "failed", "cancelled"],
                    "description": "New status for the task"
                },
                "feasibility_reasoning": {
                    "type": "string",
                    "description": "Reasoning why task is or isn't doable"
                },
                "execution_result": {
                    "type": "object",
                    "description": "JSON payload containing execution outcome details"
                }
            },
            "required": ["task_id"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        tracing::info!(
            tool = Self::NAME,
            user_id = %self.owner.user_id.0,
            user_context_id = %self.owner.user_context_id.0,
            task_id = %args.task_id,
            status = ?args.status,
            "Tool called"
        );
        let db = self.db.as_ref().ok_or(TaskToolError::NotConfigured)?;
        let tid = Uuid::parse_str(args.task_id.trim())
            .map_err(|_| TaskToolError::InvalidInput("Invalid task UUID".into()))?;

        let is_completing = args.status.as_deref() == Some("completed");

        let res = sqlx::query(
            "UPDATE tasks SET \
             status = COALESCE($1, status), \
             feasibility_reasoning = COALESCE($2, feasibility_reasoning), \
             execution_result = COALESCE($3, execution_result), \
             completed_at = CASE WHEN $4 THEN now() ELSE completed_at END, \
             user_context_id = COALESCE(user_context_id, $7), \
             updated_at = now() \
             WHERE id = $5 AND user_id = $6 \
               AND (user_context_id = $7 OR user_context_id IS NULL)",
        )
        .bind(args.status.as_deref())
        .bind(args.feasibility_reasoning.as_deref())
        .bind(args.execution_result)
        .bind(is_completing)
        .bind(tid)
        .bind(self.owner.user_id.0)
        .bind(self.owner.user_context_id.0)
        .execute(db.pool())
        .await?;

        if res.rows_affected() == 0 {
            return Err(TaskToolError::NotFound(
                "Task not found or not owned by user".into(),
            ));
        }

        Ok(json!({
            "status": "updated",
            "task_id": tid.to_string()
        }))
    }
}
