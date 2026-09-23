use crate::{db::Db, identity::ResolvedUserContext};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use std::collections::HashSet;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IntegrationProtocol {
    Mcp,
    Direct,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEffect {
    Read,
    Write,
    Mixed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapabilityDeclaration {
    pub external_key: String,
    pub effect: CapabilityEffect,
    #[serde(default)]
    pub access_needs: Vec<String>,
    #[serde(default)]
    pub data_recipients: Vec<String>,
    #[serde(default)]
    pub regions: Vec<String>,
    #[serde(default)]
    pub failure_modes: Vec<String>,
    #[serde(default)]
    pub optional_guarantees: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct RegisterIntegrationRequest {
    pub deployment_external_key: String,
    pub external_key: String,
    pub protocol: IntegrationProtocol,
    pub display_name: String,
    pub declaration_version: i32,
    pub capabilities: Vec<CapabilityDeclaration>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SetIntegrationEnabledRequest {
    pub deployment_external_key: String,
    pub external_key: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DiscoveredCapability {
    pub integration_external_key: String,
    pub protocol: IntegrationProtocol,
    pub display_name: String,
    pub declaration_version: i32,
    pub capability: CapabilityDeclaration,
    pub declaration_is_claim: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct IntegrationDeclarationVersion {
    pub version: i32,
    pub declaration: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone)]
pub struct IntegrationRegistry {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum IntegrationRegistryError {
    #[error("integration declaration is invalid")]
    Invalid,
    #[error("integration is not found")]
    NotFound,
    #[error("integration storage unavailable")]
    Database(#[from] sqlx::Error),
}

impl IntegrationRegistry {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn register(
        &self,
        request: RegisterIntegrationRequest,
    ) -> Result<(), IntegrationRegistryError> {
        validate(&request)?;
        let deployment = deployment_id(&self.db, &request.deployment_external_key).await?;
        let snapshot =
            serde_json::to_value(&request).map_err(|_| IntegrationRegistryError::Invalid)?;
        let mut tx = self.db.pool().begin().await?;
        let protocol = protocol_name(&request.protocol);
        let key = n(&request.external_key, 255).unwrap();
        let name = n(&request.display_name, 255).unwrap();
        let inserted = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO integration_definitions \
             (deployment_id,external_key,protocol,display_name,declaration_version) \
             VALUES ($1,$2,$3,$4,$5) \
             ON CONFLICT (deployment_id,external_key) DO NOTHING RETURNING id",
        )
        .bind(deployment)
        .bind(&key)
        .bind(protocol)
        .bind(&name)
        .bind(request.declaration_version)
        .fetch_optional(&mut *tx)
        .await?;
        let id = if let Some(id) = inserted {
            id
        } else {
            let existing = sqlx::query_as::<_, (Uuid, i32)>(
                "SELECT id,declaration_version FROM integration_definitions \
                 WHERE deployment_id=$1 AND external_key=$2 FOR UPDATE",
            )
            .bind(deployment)
            .bind(&key)
            .fetch_one(&mut *tx)
            .await?;
            if request.declaration_version == existing.1 {
                let stored = sqlx::query_scalar::<_, Value>(
                    "SELECT declaration FROM integration_declaration_versions \
                     WHERE integration_id=$1 AND version=$2",
                )
                .bind(existing.0)
                .bind(existing.1)
                .fetch_one(&mut *tx)
                .await?;
                if stored == snapshot {
                    return Ok(());
                }
                return Err(IntegrationRegistryError::Invalid);
            }
            if request.declaration_version < existing.1 {
                return Err(IntegrationRegistryError::Invalid);
            }
            // A new declaration may change recipients, access, guarantees, or
            // effects. Existing consent and grants cannot carry over silently.
            sqlx::query(
                "UPDATE agent_capability_grants SET state='revoked', \
                 revoked_at=COALESCE(revoked_at,now()),updated_at=now() \
                 WHERE connection_id IN \
                 (SELECT id FROM external_connections WHERE integration_id=$1) \
                 AND state='enabled'",
            )
            .bind(existing.0)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE external_connections SET authorization_state='expired', \
                 authorized_capabilities='{}'::text[],expires_at=NULL, \
                 failure_code=NULL,updated_at=now() \
                 WHERE integration_id=$1 AND authorization_state='authorized'",
            )
            .bind(existing.0)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "UPDATE integration_definitions SET protocol=$2,display_name=$3, \
                 declaration_version=$4,state='disabled',updated_at=now() WHERE id=$1",
            )
            .bind(existing.0)
            .bind(protocol)
            .bind(&name)
            .bind(request.declaration_version)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM integration_capability_declarations WHERE integration_id=$1")
                .bind(existing.0)
                .execute(&mut *tx)
                .await?;
            existing.0
        };
        sqlx::query(
            "INSERT INTO integration_declaration_versions \
             (integration_id,version,declaration) VALUES ($1,$2,$3)",
        )
        .bind(id)
        .bind(request.declaration_version)
        .bind(snapshot)
        .execute(&mut *tx)
        .await?;
        for capability in request.capabilities {
            sqlx::query("INSERT INTO integration_capability_declarations (integration_id,external_key,effect,access_needs,data_recipients,regions,failure_modes,optional_guarantees) VALUES ($1,$2,$3,$4,$5,$6,$7,$8)").bind(id).bind(n(&capability.external_key,255).unwrap()).bind(effect_name(&capability.effect)).bind(strings(capability.access_needs)?).bind(strings(capability.data_recipients)?).bind(strings(capability.regions)?).bind(strings(capability.failure_modes)?).bind(capability.optional_guarantees).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn versions(
        &self,
        deployment_external_key: &str,
        integration_external_key: &str,
    ) -> Result<Vec<IntegrationDeclarationVersion>, IntegrationRegistryError> {
        let deployment = deployment_id(&self.db, deployment_external_key).await?;
        let key = n(integration_external_key, 255).ok_or(IntegrationRegistryError::Invalid)?;
        let rows = sqlx::query(
            "SELECT v.version,v.declaration,v.created_at \
             FROM integration_declaration_versions v \
             JOIN integration_definitions i ON i.id=v.integration_id \
             WHERE i.deployment_id=$1 AND i.external_key=$2 ORDER BY v.version",
        )
        .bind(deployment)
        .bind(key)
        .fetch_all(self.db.pool())
        .await?;
        if rows.is_empty() {
            return Err(IntegrationRegistryError::NotFound);
        }
        rows.into_iter()
            .map(|row| {
                Ok(IntegrationDeclarationVersion {
                    version: row.try_get("version")?,
                    declaration: row.try_get("declaration")?,
                    created_at: row.try_get("created_at")?,
                })
            })
            .collect()
    }

    pub async fn set_enabled(
        &self,
        request: SetIntegrationEnabledRequest,
    ) -> Result<(), IntegrationRegistryError> {
        let deployment = deployment_id(&self.db, &request.deployment_external_key).await?;
        let changed=sqlx::query("UPDATE integration_definitions SET state=$3,updated_at=now() WHERE deployment_id=$1 AND external_key=$2").bind(deployment).bind(n(&request.external_key,255).ok_or(IntegrationRegistryError::Invalid)?).bind(if request.enabled{"enabled"}else{"disabled"}).execute(self.db.pool()).await?.rows_affected();
        if changed == 0 {
            Err(IntegrationRegistryError::NotFound)
        } else {
            Ok(())
        }
    }

    pub async fn discover(
        &self,
        deployment_external_key: &str,
    ) -> Result<Vec<DiscoveredCapability>, IntegrationRegistryError> {
        let deployment = deployment_id(&self.db, deployment_external_key).await?;
        self.discover_for_deployment(deployment).await
    }

    /// A host-facing catalog resolved from its signed user context, never a
    /// caller-supplied deployment key. Agent filtering is informational:
    /// declarations remain claims, and grants/execution are enforced elsewhere.
    pub async fn discover_for_context(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: Option<&str>,
        region: Option<&str>,
    ) -> Result<Vec<DiscoveredCapability>, IntegrationRegistryError> {
        let mut capabilities = self
            .discover_for_deployment(context.subject.deployment_id.0)
            .await?;
        let region = match region {
            Some(value)
                if value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_alphabetic()) =>
            {
                Some(value.to_ascii_uppercase())
            }
            Some(_) => return Err(IntegrationRegistryError::Invalid),
            None => None,
        };
        capabilities.retain(|item| {
            item.capability.regions.iter().any(|available| {
                available.eq_ignore_ascii_case("global")
                    || region
                        .as_ref()
                        .is_some_and(|region| available.eq_ignore_ascii_case(region))
            })
        });
        if let Some(agent_external_key) = agent_external_key {
            let key = n(agent_external_key, 255).ok_or(IntegrationRegistryError::Invalid)?;
            let requested = sqlx::query_scalar::<_, Vec<String>>(
                "SELECT a.requested_capability_categories FROM agent_definitions a \
                 JOIN deployment_agent_selections s ON s.agent_definition_id=a.id \
                 AND s.deployment_id=a.deployment_id \
                 WHERE a.deployment_id=$1 AND a.external_key=$2 AND a.state='enabled'",
            )
            .bind(context.subject.deployment_id.0)
            .bind(key)
            .fetch_optional(self.db.pool())
            .await?
            .ok_or(IntegrationRegistryError::NotFound)?;
            capabilities.retain(|item| {
                requested.iter().any(|category| {
                    category
                        == &format!(
                            "{}.{}",
                            item.integration_external_key, item.capability.external_key
                        )
                })
            });
        }
        Ok(capabilities)
    }

    async fn discover_for_deployment(
        &self,
        deployment: Uuid,
    ) -> Result<Vec<DiscoveredCapability>, IntegrationRegistryError> {
        let rows=sqlx::query("SELECT i.external_key,i.protocol,i.display_name,i.declaration_version,c.external_key,c.effect,c.access_needs,c.data_recipients,c.regions,c.failure_modes,c.optional_guarantees FROM integration_definitions i JOIN integration_capability_declarations c ON c.integration_id=i.id WHERE i.deployment_id=$1 AND i.state='enabled' ORDER BY i.external_key,c.external_key").bind(deployment).fetch_all(self.db.pool()).await?;
        rows.into_iter().map(row).collect()
    }
}

fn row(row: sqlx::postgres::PgRow) -> Result<DiscoveredCapability, IntegrationRegistryError> {
    Ok(DiscoveredCapability {
        integration_external_key: row.try_get(0)?,
        protocol: match row.try_get::<String, _>(1)?.as_str() {
            "mcp" => IntegrationProtocol::Mcp,
            "direct" => IntegrationProtocol::Direct,
            _ => return Err(IntegrationRegistryError::Invalid),
        },
        display_name: row.try_get(2)?,
        declaration_version: row.try_get(3)?,
        capability: CapabilityDeclaration {
            external_key: row.try_get(4)?,
            effect: match row.try_get::<String, _>(5)?.as_str() {
                "read" => CapabilityEffect::Read,
                "write" => CapabilityEffect::Write,
                "mixed" => CapabilityEffect::Mixed,
                _ => return Err(IntegrationRegistryError::Invalid),
            },
            access_needs: row.try_get(6)?,
            data_recipients: row.try_get(7)?,
            regions: row.try_get(8)?,
            failure_modes: row.try_get(9)?,
            optional_guarantees: row.try_get(10)?,
        },
        declaration_is_claim: true,
    })
}

async fn deployment_id(db: &Db, key: &str) -> Result<Uuid, IntegrationRegistryError> {
    sqlx::query_scalar("SELECT id FROM platform_deployments WHERE external_key=$1")
        .bind(n(key, 255).ok_or(IntegrationRegistryError::Invalid)?)
        .fetch_optional(db.pool())
        .await?
        .ok_or(IntegrationRegistryError::NotFound)
}

fn validate(request: &RegisterIntegrationRequest) -> Result<(), IntegrationRegistryError> {
    if n(&request.external_key, 255).is_none()
        || n(&request.display_name, 255).is_none()
        || request.declaration_version < 1
        || request.capabilities.is_empty()
        || request.capabilities.len() > 128
    {
        return Err(IntegrationRegistryError::Invalid);
    };
    let mut keys = HashSet::new();
    for c in &request.capabilities {
        let key = n(&c.external_key, 255).ok_or(IntegrationRegistryError::Invalid)?;
        if !keys.insert(key)
            || !c.optional_guarantees.is_object()
            || contains_sensitive_key(&c.optional_guarantees)
            || c.access_needs.len() > 64
            || c.data_recipients.len() > 64
            || c.regions.len() > 64
            || c.failure_modes.len() > 64
        {
            return Err(IntegrationRegistryError::Invalid);
        }
    }
    Ok(())
}

fn contains_sensitive_key(value: &Value) -> bool {
    match value {
        Value::Object(fields) => fields.iter().any(|(key, value)| {
            let key = key.to_ascii_lowercase();
            [
                "secret",
                "token",
                "password",
                "credential",
                "api_key",
                "private_key",
            ]
            .iter()
            .any(|part| key.contains(part))
                || contains_sensitive_key(value)
        }),
        Value::Array(values) => values.iter().any(contains_sensitive_key),
        _ => false,
    }
}

fn n(v: &str, max: usize) -> Option<String> {
    let v = v.trim();
    (!v.is_empty() && v.len() <= max).then(|| v.to_owned())
}

fn strings(v: Vec<String>) -> Result<Vec<String>, IntegrationRegistryError> {
    if v.len() > 64 {
        return Err(IntegrationRegistryError::Invalid);
    }
    v.into_iter()
        .map(|x| n(&x, 255).ok_or(IntegrationRegistryError::Invalid))
        .collect()
}

fn protocol_name(v: &IntegrationProtocol) -> &'static str {
    match v {
        IntegrationProtocol::Mcp => "mcp",
        IntegrationProtocol::Direct => "direct",
    }
}

fn effect_name(v: &CapabilityEffect) -> &'static str {
    match v {
        CapabilityEffect::Read => "read",
        CapabilityEffect::Write => "write",
        CapabilityEffect::Mixed => "mixed",
    }
}
