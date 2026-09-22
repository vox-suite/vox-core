/**
* Application service coordinating asynchronous and durable task execution.
*/
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{identity::Actor, tasks::Task},
    storage::tasks::TaskRepository,
};

#[derive(Debug, Deserialize)]
pub struct CreateTaskInput {
    pub collection_id: Option<Uuid>,
    pub title: String,
    pub instruction: String,
    pub priority: Option<i32>,
    pub due_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct TaskService {
    repo: TaskRepository,
}

impl TaskService {
    pub fn new(repo: TaskRepository) -> Self {
        Self { repo }
    }

    pub async fn create_task(
        &self,
        actor: &Actor,
        input: CreateTaskInput,
    ) -> Result<Task, sqlx::Error> {
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
