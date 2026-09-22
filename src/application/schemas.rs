/**
* Application service managing JSON schema registrations and validation.
*/
use serde::Deserialize;

use crate::{
    domain::{identity::Actor, schemas::DataSchema},
    storage::schemas::SchemaRepository,
};

#[derive(Debug, Deserialize)]
pub struct CreateSchemaVersionInput {
    pub namespace: String,
    pub name: String,
    pub version: i32,
    pub description: Option<String>,
    pub json_schema: serde_json::Value,
}

#[derive(Clone)]
pub struct SchemaService {
    repo: SchemaRepository,
}

impl SchemaService {
    pub fn new(repo: SchemaRepository) -> Self {
        Self { repo }
    }

    pub async fn create_version(
        &self,
        actor: &Actor,
        input: CreateSchemaVersionInput,
    ) -> Result<DataSchema, sqlx::Error> {
        self.repo
            .create_version(
                Some(actor.user_id),
                &input.namespace,
                &input.name,
                input.version,
                input.description.as_deref().unwrap_or(""),
                input.json_schema,
            )
            .await
    }

    pub async fn get_by_name(
        &self,
        actor: &Actor,
        namespace: &str,
        name: &str,
    ) -> Result<Option<DataSchema>, sqlx::Error> {
        self.repo
            .get_by_name(Some(actor.user_id), namespace, name)
            .await
    }
}
