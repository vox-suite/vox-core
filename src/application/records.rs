/**
* Application service coordinating record validation, storage, and search.
*/
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

use crate::{
    domain::{
        identity::Actor,
        records::{Record, RecordKind},
    },
    storage::{
        collections::CollectionRepository, records::RecordRepository, schemas::SchemaRepository,
    },
};

#[derive(Debug, thiserror::Error)]
pub enum RecordServiceError {
    #[error("schema not found")]
    SchemaNotFound,
    #[error("schema definition is invalid: {0}")]
    InvalidSchema(String),
    #[error("schema validation failed: {0}")]
    ValidationError(String),
    #[error("collection not found")]
    CollectionNotFound,
    #[error("record not found")]
    NotFound,
    #[error("optimistic concurrency conflict")]
    VersionConflict,
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
}

#[derive(Debug, Deserialize)]
pub struct CreateRecordInput {
    pub schema_id: Uuid,
    pub schema_scope: Option<String>,
    pub kind: Option<RecordKind>,
    pub domain: String,
    pub entity_type: String,
    pub title: String,
    pub data: serde_json::Value,
    pub source: Option<String>,
    pub occurred_at: Option<DateTime<Utc>>,
    pub collection_id: Option<Uuid>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UpdateRecordInput {
    pub expected_version: Option<i32>,
    pub title: Option<String>,
    pub data: Option<serde_json::Value>,
}

#[derive(Clone)]
pub struct RecordService {
    repo: RecordRepository,
    schemas: SchemaRepository,
    collections: CollectionRepository,
}

impl RecordService {
    pub fn new(
        repo: RecordRepository,
        schemas: SchemaRepository,
        collections: CollectionRepository,
    ) -> Self {
        Self {
            repo,
            schemas,
            collections,
        }
    }

    pub async fn create_record(
        &self,
        actor: &Actor,
        input: CreateRecordInput,
    ) -> Result<Record, RecordServiceError> {
        if let Some(col_id) = input.collection_id {
            let col = self.collections.get_by_id(actor.user_id, col_id).await?;
            if col.is_none() {
                return Err(RecordServiceError::CollectionNotFound);
            }
        }

        let schema = self
            .schemas
            .get_by_id(input.schema_id)
            .await?
            .ok_or(RecordServiceError::SchemaNotFound)?;

        if schema.user_id.is_some() && schema.user_id != Some(actor.user_id) {
            return Err(RecordServiceError::SchemaNotFound);
        }

        let validator = crate::application::schemas::compile_schema(&schema.json_schema)
            .map_err(RecordServiceError::InvalidSchema)?;

        if let Err(error) = validator.validate(&input.data) {
            return Err(RecordServiceError::ValidationError(error.to_string()));
        }

        let schema_scope = input.schema_scope.unwrap_or_else(|| "global".to_string());
        self.repo
            .create(
                actor.user_id,
                input.schema_id,
                &schema_scope,
                input.kind.unwrap_or(RecordKind::Fact),
                &input.domain,
                &input.entity_type,
                &input.title,
                input.data,
                input.source.as_deref().unwrap_or("api"),
                input.occurred_at,
                input.collection_id,
            )
            .await
            .map_err(RecordServiceError::from)
    }

    pub async fn update_record(
        &self,
        actor: &Actor,
        id: Uuid,
        input: UpdateRecordInput,
    ) -> Result<Record, RecordServiceError> {
        if let Some(ref new_data) = input.data {
            let existing = self
                .repo
                .get_by_id(actor.user_id, id)
                .await?
                .ok_or(RecordServiceError::NotFound)?;
            let schema = self
                .schemas
                .get_by_id(existing.schema_id)
                .await?
                .ok_or(RecordServiceError::SchemaNotFound)?;
            let validator = crate::application::schemas::compile_schema(&schema.json_schema)
                .map_err(RecordServiceError::InvalidSchema)?;
            if let Err(error) = validator.validate(new_data) {
                return Err(RecordServiceError::ValidationError(error.to_string()));
            }
        }

        let outcome = self
            .repo
            .update(
                actor.user_id,
                id,
                input.expected_version,
                input.title.as_deref(),
                input.data,
            )
            .await?;

        match outcome {
            crate::domain::ConcurrencyOutcome::Success(rec) => Ok(rec),
            crate::domain::ConcurrencyOutcome::Conflict => Err(RecordServiceError::VersionConflict),
            crate::domain::ConcurrencyOutcome::NotFound => Err(RecordServiceError::NotFound),
        }
    }

    pub async fn get_record(&self, actor: &Actor, id: Uuid) -> Result<Option<Record>, sqlx::Error> {
        self.repo.get_by_id(actor.user_id, id).await
    }

    pub async fn list_records(
        &self,
        actor: &Actor,
        domain: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Record>, sqlx::Error> {
        self.repo.list(actor.user_id, domain, limit).await
    }

    pub async fn delete_record(&self, actor: &Actor, id: Uuid) -> Result<bool, sqlx::Error> {
        self.repo.delete(actor.user_id, id).await
    }
}
