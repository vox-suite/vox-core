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
    storage::records::RecordRepository,
};

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
}

#[derive(Clone)]
pub struct RecordService {
    repo: RecordRepository,
}

impl RecordService {
    pub fn new(repo: RecordRepository) -> Self {
        Self { repo }
    }

    pub async fn create_record(
        &self,
        actor: &Actor,
        input: CreateRecordInput,
    ) -> Result<Record, sqlx::Error> {
        let schema_scope = input
            .schema_scope
            .unwrap_or_else(|| "global".to_string());
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
            )
            .await
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
