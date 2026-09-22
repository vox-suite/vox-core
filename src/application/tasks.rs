/**
* Application service coordinating asynchronous and durable task execution.
*/
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{identity::Actor, tasks::{Task, TaskStatus}},
    storage::{collections::CollectionRepository, tasks::TaskRepository},
};

#[derive(Debug, thiserror::Error)]
pub enum TaskServiceError {
    #[error("collection not found")]
    CollectionNotFound,
    #[error("task not found")]
    NotFound,
    #[error("optimistic concurrency conflict")]
    VersionConflict,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Deserialize)]
pub struct CreateTaskInput {
    pub collection_id: Option<Uuid>,
    pub title: String,
    pub instruction: String,
    pub priority: Option<i32>,
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateTaskInput {
    pub expected_version: Option<i32>,
    pub title: Option<String>,
    pub instruction: Option<String>,
    pub status: Option<TaskStatus>,
    pub priority: Option<i32>,
    pub due_at: Option<Option<DateTime<Utc>>>,
    pub feasibility_reasoning: Option<String>,
    pub execution_result: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct TaskService {
    repo: TaskRepository,
    collections: CollectionRepository,
}

impl TaskService {
    pub fn new(repo: TaskRepository, collections: CollectionRepository) -> Self {
        Self { repo, collections }
    }

    pub async fn create_task(
        &self,
        actor: &Actor,
        input: CreateTaskInput,
    ) -> Result<Task, TaskServiceError> {
        if let Some(col_id) = input.collection_id {
            let col = self.collections.get_by_id(actor.user_id, col_id).await?;
            if col.is_none() {
                return Err(TaskServiceError::CollectionNotFound);
            }
        }

        self.repo
            .create(
                actor.user_id,
                input.collection_id,
                &input.title,
                &input.instruction,
                input.priority.unwrap_or(0),
                input.due_at,
            )
            .await
            .map_err(TaskServiceError::from)
    }

    pub async fn update_task(
        &self,
        actor: &Actor,
        id: Uuid,
        input: UpdateTaskInput,
    ) -> Result<Task, TaskServiceError> {
        let outcome = self
            .repo
            .update(
                actor.user_id,
                id,
                input.expected_version,
                input.title.as_deref(),
                input.instruction.as_deref(),
                input.status,
                input.priority,
                input.due_at,
                input.feasibility_reasoning.as_deref(),
                input.execution_result,
            )
            .await?;

        match outcome {
            crate::domain::ConcurrencyOutcome::Success(task) => Ok(task),
            crate::domain::ConcurrencyOutcome::Conflict => Err(TaskServiceError::VersionConflict),
            crate::domain::ConcurrencyOutcome::NotFound => Err(TaskServiceError::NotFound),
        }
    }

    pub async fn get_task(&self, actor: &Actor, id: Uuid) -> Result<Option<Task>, sqlx::Error> {
        self.repo.get_by_id(actor.user_id, id).await
    }

    pub async fn list_tasks(&self, actor: &Actor, limit: i64) -> Result<Vec<Task>, sqlx::Error> {
        self.repo.list(actor.user_id, limit).await
    }

    pub async fn delete_task(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        self.repo.delete(actor.user_id, id).await
    }
}
