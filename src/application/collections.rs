/**
* Application service managing collections and the spans they group.
*/
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{
        collections::{Collection, CollectionKind, CollectionStatus},
        identity::Actor,
        spans::present,
    },
    storage::collections::CollectionRepository,
};

#[derive(Debug, thiserror::Error)]
pub enum CollectionServiceError {
    #[error("invalid collection: {0}")]
    Invalid(&'static str),
    #[error("collection or span not found")]
    NotFound,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Deserialize)]
pub struct CreateCollectionInput {
    pub name: String,
    pub description: Option<String>,
    pub kind: Option<CollectionKind>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub metadata: Option<serde_json::Value>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateCollectionInput {
    pub name: Option<String>,
    pub description: Option<String>,
    pub status: Option<CollectionStatus>,
    #[serde(default, deserialize_with = "present")]
    pub starts_at: Option<Option<DateTime<Utc>>>,
    #[serde(default, deserialize_with = "present")]
    pub ends_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Clone)]
pub struct CollectionService {
    repo: CollectionRepository,
}

impl CollectionService {
    pub fn new(repo: CollectionRepository) -> Self {
        Self { repo }
    }

    pub async fn create_collection(
        &self,
        actor: &Actor,
        input: CreateCollectionInput,
    ) -> Result<Collection, CollectionServiceError> {
        if input.name.trim().is_empty() {
            return Err(CollectionServiceError::Invalid("name must not be empty"));
        }
        if let (Some(start), Some(end)) = (input.starts_at, input.ends_at)
            && end < start
        {
            return Err(CollectionServiceError::Invalid(
                "ends_at must not precede starts_at",
            ));
        }
        Ok(self
            .repo
            .create(
                actor.user_id,
                &input.name,
                input.description.as_deref().unwrap_or(""),
                input.kind.unwrap_or_default(),
                input.starts_at,
                input.ends_at,
                input.metadata.unwrap_or_else(|| serde_json::json!({})),
            )
            .await?)
    }

    pub async fn get_collection(
        &self,
        actor: &Actor,
        id: Uuid,
    ) -> Result<Option<Collection>, sqlx::Error> {
        self.repo.get_by_id(actor.user_id, id).await
    }

    pub async fn list_collections(
        &self,
        actor: &Actor,
        limit: i64,
    ) -> Result<Vec<Collection>, sqlx::Error> {
        self.repo.list(actor.user_id, limit).await
    }

    pub async fn update_collection(
        &self,
        actor: &Actor,
        id: Uuid,
        input: UpdateCollectionInput,
    ) -> Result<Collection, CollectionServiceError> {
        if input.name.as_deref().is_some_and(|n| n.trim().is_empty()) {
            return Err(CollectionServiceError::Invalid("name must not be empty"));
        }
        self.repo
            .update(
                actor.user_id,
                id,
                input.name.as_deref(),
                input.description.as_deref(),
                input.status,
                input.starts_at,
                input.ends_at,
            )
            .await
            .map_err(check_violation)?
            .ok_or(CollectionServiceError::NotFound)
    }

    pub async fn archive_collection(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        self.repo.archive(actor.user_id, id).await
    }

    pub async fn add_span(
        &self,
        actor: &Actor,
        collection_id: Uuid,
        span_id: Uuid,
    ) -> Result<(), CollectionServiceError> {
        self.repo
            .add_span(actor.user_id, collection_id, span_id)
            .await
            .map_err(|err| match &err {
                sqlx::Error::Database(db) if db.code().as_deref() == Some("23503") => {
                    CollectionServiceError::NotFound
                }
                _ => CollectionServiceError::Database(err),
            })
    }

    pub async fn remove_span(
        &self,
        actor: &Actor,
        collection_id: Uuid,
        span_id: Uuid,
    ) -> Result<bool, sqlx::Error> {
        self.repo
            .remove_span(actor.user_id, collection_id, span_id)
            .await
    }
}

fn check_violation(err: sqlx::Error) -> CollectionServiceError {
    match &err {
        sqlx::Error::Database(db) if db.code().as_deref() == Some("23514") => {
            CollectionServiceError::Invalid("ends_at must not precede starts_at")
        }
        _ => CollectionServiceError::Database(err),
    }
}
