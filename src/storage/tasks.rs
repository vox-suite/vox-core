/**
 * Storage repository for asynchronous tasks and execution states.
 */

use chrono::{DateTime, Utc};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::tasks::{ExecutionType, Task, TaskStatus};

#[derive(Clone)]
pub struct TaskRepository {
    pool: PgPool,
}

impl TaskRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        user_id: Uuid,
        collection_id: Option<Uuid>,
        title: &str,
        instruction: &str,
        priority: i32,
        due_at: Option<DateTime<Utc>>,
    ) -> Result<Task, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO tasks (user_id, collection_id, title, instruction, priority, due_at)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, user_id, collection_id, title, instruction, status, priority,
                      execution_type, feasibility_reasoning, execution_result, due_at,
                      version, cancellation_requested_at, created_at, updated_at, completed_at
            "#,
        )
        .bind(user_id)
        .bind(collection_id)
        .bind(title)
        .bind(instruction)
        .bind(priority)
        .bind(due_at)
        .fetch_one(&self.pool)
        .await?;

        let status_str: String = row.get("status");
        let exec_str: String = row.get("execution_type");

        Ok(Task {
            id: row.get("id"),
            user_id: row.get("user_id"),
            collection_id: row.get("collection_id"),
            title: row.get("title"),
            instruction: row.get("instruction"),
            status: match status_str.as_str() {
                "evaluating" => TaskStatus::Evaluating,
                "executing" => TaskStatus::Executing,
                "waiting_user" => TaskStatus::WaitingUser,
                "completed" => TaskStatus::Completed,
                "failed" => TaskStatus::Failed,
                "cancelled" => TaskStatus::Cancelled,
                _ => TaskStatus::Pending,
            },
            priority: row.get("priority"),
            execution_type: match exec_str.as_str() {
                "autonomous" => ExecutionType::Autonomous,
                "interactive" => ExecutionType::Interactive,
                _ => ExecutionType::ManualHuman,
            },
            feasibility_reasoning: row.get("feasibility_reasoning"),
            execution_result: row.get("execution_result"),
            due_at: row.get("due_at"),
            version: row.get("version"),
            cancellation_requested_at: row.get("cancellation_requested_at"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
            completed_at: row.get("completed_at"),
        })
    }

    pub async fn get_by_id(&self, user_id: Uuid, id: Uuid) -> Result<Option<Task>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, collection_id, title, instruction, status, priority,
                   execution_type, feasibility_reasoning, execution_result, due_at,
                   version, cancellation_requested_at, created_at, updated_at, completed_at
            FROM tasks
            WHERE id = $1 AND user_id = $2
            "#,
        )
        .bind(id)
        .bind(user_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let status_str: String = r.get("status");
            let exec_str: String = r.get("execution_type");
            Task {
                id: r.get("id"),
                user_id: r.get("user_id"),
                collection_id: r.get("collection_id"),
                title: r.get("title"),
                instruction: r.get("instruction"),
                status: match status_str.as_str() {
                    "evaluating" => TaskStatus::Evaluating,
                    "executing" => TaskStatus::Executing,
                    "waiting_user" => TaskStatus::WaitingUser,
                    "completed" => TaskStatus::Completed,
                    "failed" => TaskStatus::Failed,
                    "cancelled" => TaskStatus::Cancelled,
                    _ => TaskStatus::Pending,
                },
                priority: r.get("priority"),
                execution_type: match exec_str.as_str() {
                    "autonomous" => ExecutionType::Autonomous,
                    "interactive" => ExecutionType::Interactive,
                    _ => ExecutionType::ManualHuman,
                },
                feasibility_reasoning: r.get("feasibility_reasoning"),
                execution_result: r.get("execution_result"),
                due_at: r.get("due_at"),
                version: r.get("version"),
                cancellation_requested_at: r.get("cancellation_requested_at"),
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
                completed_at: r.get("completed_at"),
            }
        }))
    }

    pub async fn list(&self, user_id: Uuid, limit: i64) -> Result<Vec<Task>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, user_id, collection_id, title, instruction, status, priority,
                   execution_type, feasibility_reasoning, execution_result, due_at,
                   version, cancellation_requested_at, created_at, updated_at, completed_at
            FROM tasks
            WHERE user_id = $1
            ORDER BY created_at DESC
            LIMIT $2
            "#,
        )
        .bind(user_id)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows
            .into_iter()
            .map(|r| {
                let status_str: String = r.get("status");
                let exec_str: String = r.get("execution_type");
                Task {
                    id: r.get("id"),
                    user_id: r.get("user_id"),
                    collection_id: r.get("collection_id"),
                    title: r.get("title"),
                    instruction: r.get("instruction"),
                    status: match status_str.as_str() {
                        "evaluating" => TaskStatus::Evaluating,
                        "executing" => TaskStatus::Executing,
                        "waiting_user" => TaskStatus::WaitingUser,
                        "completed" => TaskStatus::Completed,
                        "failed" => TaskStatus::Failed,
                        "cancelled" => TaskStatus::Cancelled,
                        _ => TaskStatus::Pending,
                    },
                    priority: r.get("priority"),
                    execution_type: match exec_str.as_str() {
                        "autonomous" => ExecutionType::Autonomous,
                        "interactive" => ExecutionType::Interactive,
                        _ => ExecutionType::ManualHuman,
                    },
                    feasibility_reasoning: r.get("feasibility_reasoning"),
                    execution_result: r.get("execution_result"),
                    due_at: r.get("due_at"),
                    version: r.get("version"),
                    cancellation_requested_at: r.get("cancellation_requested_at"),
                    created_at: r.get("created_at"),
                    updated_at: r.get("updated_at"),
                    completed_at: r.get("completed_at"),
                }
            })
            .collect())
    }

    pub async fn delete(&self, user_id: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query("DELETE FROM tasks WHERE id = $1 AND user_id = $2")
            .bind(id)
            .bind(user_id)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected() > 0)
    }
}
