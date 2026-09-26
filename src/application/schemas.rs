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

#[derive(Debug, thiserror::Error)]
pub enum SchemaServiceError {
    #[error("invalid JSON schema definition: {0}")]
    InvalidSchema(String),
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),
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
    ) -> Result<DataSchema, SchemaServiceError> {
        compile_schema(&input.json_schema).map_err(SchemaServiceError::InvalidSchema)?;

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
            .map_err(SchemaServiceError::from)
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

pub fn compile_schema(schema: &serde_json::Value) -> Result<jsonschema::Validator, String> {
    reject_external_refs(schema)?;
    jsonschema::options()
        .offline()
        .build(schema)
        .map_err(|error| error.to_string())
}

fn reject_external_refs(schema: &serde_json::Value) -> Result<(), String> {
    match schema {
        serde_json::Value::Array(items) => {
            for item in items {
                reject_external_refs(item)?;
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(reference) = map.get("$ref").and_then(|value| value.as_str())
                && !is_local_schema_ref(reference)
            {
                return Err(format!(
                    "external schema reference is not allowed: {reference}"
                ));
            }
            for value in map.values() {
                reject_external_refs(value)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_local_schema_ref(reference: &str) -> bool {
    reference == "#" || reference.starts_with("#/")
}
