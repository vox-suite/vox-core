/**
* Application service managing dynamic record collections and schemas.
*/
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{
        collections::{Collection, CollectionKind},
        identity::Actor,
    },
    storage::collections::CollectionRepository,
};

#[derive(Debug, Deserialize)]
pub struct CreateCollectionInput {
    pub name: String,
    pub description: Option<String>,
    pub kind: Option<CollectionKind>,
    pub metadata: Option<serde_json::Value>,
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
    ) -> Result<Collection, sqlx::Error> {
        self.repo
            .create(
                actor.user_id,
                &input.name,
                input.description.as_deref().unwrap_or(""),
                input.kind.unwrap_or_default(),
                input.metadata.unwrap_or_else(|| serde_json::json!({})),
            )
            .await
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

    pub async fn archive_collection(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        self.repo.archive(actor.user_id, id).await
    }
}
