/**
* Agent registry service managing agent declarations and capability manifests.
*/
use crate::{db::Db, identity::DeploymentId};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const MAX_CAPABILITY_CATEGORIES: usize = 64;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RegisterAgentDefinitionRequest {
    pub deployment_external_key: String,
    pub external_key: String,
    pub purpose: String,
    #[serde(default)]
    pub requested_capability_categories: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelConfigurationRequest {
    pub model_adapter: String,
    pub model: String,
    #[serde(default)]
    pub configuration: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SelectAgentRequest {
    pub deployment_external_key: String,
    pub agent_external_key: String,
    pub model_configuration: ModelConfigurationRequest,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetAgentEnabledRequest {
    pub deployment_external_key: String,
    pub agent_external_key: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AgentDefinition {
    pub id: Uuid,
    pub deployment_id: DeploymentId,
    pub external_key: String,
    pub purpose: String,
    pub requested_capability_categories: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelConfiguration {
    pub id: Uuid,
    pub version: i32,
    pub model_adapter: String,
    pub model: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SelectedAgent {
    pub definition: AgentDefinition,
    pub model_configuration: ModelConfiguration,
}

#[derive(Clone)]
pub struct AgentRegistry {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentRegistryError {
    #[error("agent definition is invalid")]
    InvalidDefinition,
    #[error("model configuration is invalid")]
    InvalidModelConfiguration,
    #[error("model configuration must not contain credentials")]
    SensitiveModelConfiguration,
    #[error("agent definition is not available")]
    NotFound,
    #[error("agent registry storage is unavailable")]
    Database(#[from] sqlx::Error),
}

impl AgentRegistry {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn register(
        &self,
        request: RegisterAgentDefinitionRequest,
    ) -> Result<AgentDefinition, AgentRegistryError> {
        let deployment_key = normalize(&request.deployment_external_key, 255)
            .ok_or(AgentRegistryError::InvalidDefinition)?;
        let external_key =
            normalize(&request.external_key, 255).ok_or(AgentRegistryError::InvalidDefinition)?;
        let purpose =
            normalize(&request.purpose, 2048).ok_or(AgentRegistryError::InvalidDefinition)?;
        let categories = normalize_categories(request.requested_capability_categories)?;
        let deployment_id = deployment_id(&self.db, &deployment_key).await?;
        let id = match sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO agent_definitions (deployment_id, external_key, purpose, requested_capability_categories) \
             VALUES ($1, $2, $3, $4) \
             ON CONFLICT (deployment_id, external_key) DO UPDATE \
             SET purpose = EXCLUDED.purpose, requested_capability_categories = EXCLUDED.requested_capability_categories, \
                 state = 'enabled', updated_at = now() \
             RETURNING id",
        )
        .bind(deployment_id)
        .bind(&external_key)
        .bind(&purpose)
        .bind(&categories)
        .fetch_one(self.db.pool())
        .await
        {
            Ok(id) => id,
            Err(error) if missing_table(&error) => return Err(AgentRegistryError::NotFound),
            Err(error) => return Err(error.into()),
        };
        Ok(AgentDefinition {
            id,
            deployment_id: DeploymentId(deployment_id),
            external_key,
            purpose,
            requested_capability_categories: categories,
        })
    }

    pub async fn select(
        &self,
        request: SelectAgentRequest,
    ) -> Result<SelectedAgent, AgentRegistryError> {
        let deployment_key = normalize(&request.deployment_external_key, 255)
            .ok_or(AgentRegistryError::InvalidDefinition)?;
        let external_key = normalize(&request.agent_external_key, 255)
            .ok_or(AgentRegistryError::InvalidDefinition)?;
        validate_model_configuration(&request.model_configuration)?;
        let mut tx = self.db.pool().begin().await?;
        let deployment_id = deployment_id_tx(&mut tx, &deployment_key).await?;
        let definition = load_definition(&mut tx, deployment_id, &external_key).await?;
        let version = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(version), 0) + 1 FROM agent_model_configurations WHERE agent_definition_id = $1",
        )
        .bind(definition.id)
        .fetch_one(&mut *tx)
        .await?;
        let configuration_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO agent_model_configurations (agent_definition_id, version, model_adapter, model, configuration) \
             VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(definition.id)
        .bind(version)
        .bind(request.model_configuration.model_adapter.trim())
        .bind(request.model_configuration.model.trim())
        .bind(&request.model_configuration.configuration)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO deployment_agent_selections (deployment_id, agent_definition_id, model_configuration_id) \
             VALUES ($1, $2, $3) ON CONFLICT (deployment_id, agent_definition_id) \
             DO UPDATE SET model_configuration_id = EXCLUDED.model_configuration_id, selected_at = now()",
        )
        .bind(deployment_id)
        .bind(definition.id)
        .bind(configuration_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(SelectedAgent {
            definition,
            model_configuration: ModelConfiguration {
                id: configuration_id,
                version,
                model_adapter: request.model_configuration.model_adapter.trim().into(),
                model: request.model_configuration.model.trim().into(),
            },
        })
    }

    pub async fn selected_for_deployment(
        &self,
        deployment_external_key: &str,
    ) -> Result<Vec<SelectedAgent>, AgentRegistryError> {
        let key =
            normalize(deployment_external_key, 255).ok_or(AgentRegistryError::InvalidDefinition)?;
        let deployment_id = match deployment_id(&self.db, &key).await {
            Err(AgentRegistryError::NotFound) => return Ok(vec![]),
            other => other?,
        };
        let rows = match sqlx::query_as::<_, (Uuid, String, String, Vec<String>, Uuid, i32, String, String)>(
            "SELECT d.id, d.external_key, d.purpose, d.requested_capability_categories, c.id, c.version, c.model_adapter, c.model \
             FROM deployment_agent_selections s \
             JOIN agent_definitions d ON d.id = s.agent_definition_id \
             JOIN agent_model_configurations c ON c.id = s.model_configuration_id \
             WHERE s.deployment_id = $1 AND d.state = 'enabled' ORDER BY d.external_key",
        )
        .bind(deployment_id)
        .fetch_all(self.db.pool())
        .await
        {
            Ok(rows) => rows,
            Err(error) if missing_table(&error) => return Ok(vec![]),
            Err(error) => return Err(error.into()),
        };
        Ok(rows
            .into_iter()
            .map(
                |(
                    id,
                    external_key,
                    purpose,
                    requested_capability_categories,
                    model_id,
                    version,
                    model_adapter,
                    model,
                )| SelectedAgent {
                    definition: AgentDefinition {
                        id,
                        deployment_id: DeploymentId(deployment_id),
                        external_key,
                        purpose,
                        requested_capability_categories,
                    },
                    model_configuration: ModelConfiguration {
                        id: model_id,
                        version,
                        model_adapter,
                        model,
                    },
                },
            )
            .collect())
    }

    pub async fn set_enabled(
        &self,
        request: SetAgentEnabledRequest,
    ) -> Result<(), AgentRegistryError> {
        let deployment_key = normalize(&request.deployment_external_key, 255)
            .ok_or(AgentRegistryError::InvalidDefinition)?;
        let external_key = normalize(&request.agent_external_key, 255)
            .ok_or(AgentRegistryError::InvalidDefinition)?;
        let deployment_id = deployment_id(&self.db, &deployment_key).await?;
        let changed = match sqlx::query(
            "UPDATE agent_definitions SET state = $3, updated_at = now() \
             WHERE deployment_id = $1 AND external_key = $2",
        )
        .bind(deployment_id)
        .bind(external_key)
        .bind(if request.enabled {
            "enabled"
        } else {
            "disabled"
        })
        .execute(self.db.pool())
        .await
        {
            Ok(result) => result.rows_affected(),
            Err(error) if missing_table(&error) => return Err(AgentRegistryError::NotFound),
            Err(error) => return Err(error.into()),
        };
        if changed == 0 {
            Err(AgentRegistryError::NotFound)
        } else {
            Ok(())
        }
    }
}

fn missing_table(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::Database(db) if db.code().as_deref() == Some("42P01")
    )
}

async fn deployment_id(db: &Db, key: &str) -> Result<Uuid, AgentRegistryError> {
    match sqlx::query_scalar("SELECT id FROM platform_deployments WHERE external_key = $1")
        .bind(key)
        .fetch_optional(db.pool())
        .await
    {
        Ok(Some(id)) => Ok(id),
        Ok(None) => Err(AgentRegistryError::NotFound),
        Err(error) if missing_table(&error) => Err(AgentRegistryError::NotFound),
        Err(error) => Err(error.into()),
    }
}

async fn deployment_id_tx(
    tx: &mut Transaction<'_, Postgres>,
    key: &str,
) -> Result<Uuid, AgentRegistryError> {
    sqlx::query_scalar("SELECT id FROM platform_deployments WHERE external_key = $1 FOR UPDATE")
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or(AgentRegistryError::NotFound)
}

async fn load_definition(
    tx: &mut Transaction<'_, Postgres>,
    deployment_id: Uuid,
    key: &str,
) -> Result<AgentDefinition, AgentRegistryError> {
    let row = sqlx::query_as::<_, (Uuid, String, String, Vec<String>)>(
        "SELECT id, external_key, purpose, requested_capability_categories FROM agent_definitions \
         WHERE deployment_id = $1 AND external_key = $2 AND state = 'enabled' FOR UPDATE",
    )
    .bind(deployment_id)
    .bind(key)
    .fetch_optional(&mut **tx)
    .await?
    .ok_or(AgentRegistryError::NotFound)?;
    Ok(AgentDefinition {
        id: row.0,
        deployment_id: DeploymentId(deployment_id),
        external_key: row.1,
        purpose: row.2,
        requested_capability_categories: row.3,
    })
}

fn normalize(value: &str, max: usize) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= max).then(|| value.to_owned())
}

fn normalize_categories(values: Vec<String>) -> Result<Vec<String>, AgentRegistryError> {
    if values.len() > MAX_CAPABILITY_CATEGORIES {
        return Err(AgentRegistryError::InvalidDefinition);
    }
    let mut values: Vec<_> = values
        .into_iter()
        .map(|value| normalize(&value, 255))
        .collect::<Option<_>>()
        .ok_or(AgentRegistryError::InvalidDefinition)?;
    values.sort();
    values.dedup();
    Ok(values)
}

fn validate_model_configuration(
    configuration: &ModelConfigurationRequest,
) -> Result<(), AgentRegistryError> {
    normalize(&configuration.model_adapter, 255)
        .ok_or(AgentRegistryError::InvalidModelConfiguration)?;
    normalize(&configuration.model, 255).ok_or(AgentRegistryError::InvalidModelConfiguration)?;
    if !configuration.configuration.is_object() {
        return Err(AgentRegistryError::InvalidModelConfiguration);
    }
    if contains_sensitive_key(&configuration.configuration) {
        return Err(AgentRegistryError::SensitiveModelConfiguration);
    }
    Ok(())
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(values) => values.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            key.contains("secret")
                || key.contains("token")
                || key.contains("password")
                || key.contains("credential")
                || contains_sensitive_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_key),
        _ => false,
    }
}
