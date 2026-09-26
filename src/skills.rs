//! Versioned, declarative skills. A skill can guide an agent, but carries no
//! connection, grant, credential, or action approval authority.
use crate::{
    capability_grants::{CapabilityGrantError, CapabilityGrantService},
    db::Db,
    identity::ResolvedUserContext,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PublishSkillRequest {
    pub external_key: String,
    pub title: String,
    pub summary: String,
    pub instructions: String,
    #[serde(default)]
    pub requested_capabilities: Vec<String>,
    #[serde(default = "empty_resources")]
    pub resources: Value,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillListing {
    pub id: Uuid,
    pub external_key: String,
    pub title: String,
    pub summary: String,
    pub curated: bool,
    pub latest_version: i32,
    pub installed_version: Option<i32>,
    pub enabled: bool,
    pub update_available: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct EffectiveSkill {
    pub id: Uuid,
    pub external_key: String,
    pub title: String,
    pub summary: String,
    pub version: i32,
    pub requested_capabilities: Vec<String>,
    pub available_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct LoadedSkill {
    pub id: Uuid,
    pub external_key: String,
    pub title: String,
    pub version: i32,
    pub instructions: String,
    pub resources: Value,
    pub available_capabilities: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct SkillVersion {
    pub version: i32,
    pub instructions: String,
    pub requested_capabilities: Vec<String>,
    pub resources: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum SkillError {
    #[error("invalid skill declaration")]
    Invalid,
    #[error("skill unavailable")]
    NotFound,
    #[error("skill changed since review")]
    Conflict,
    #[error("skill storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("skill grant lookup unavailable")]
    Grant(#[from] CapabilityGrantError),
}

#[derive(Clone)]
pub struct SkillService {
    db: Db,
}

impl SkillService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn publish_private(
        &self,
        context: &ResolvedUserContext,
        request: PublishSkillRequest,
    ) -> Result<SkillListing, SkillError> {
        let id = self
            .publish(context.subject.deployment_id.0, Some(context.id.0), request)
            .await?;
        let row = sqlx::query(
            "SELECT s.id,s.external_key,s.title,s.summary,s.latest_version,
                    i.installed_version,i.enabled
             FROM skill_packages s JOIN skill_installations i
               ON i.skill_id=s.id AND i.user_context_id=$2
             WHERE s.id=$1 AND s.owner_user_context_id=$2",
        )
        .bind(id)
        .bind(context.id.0)
        .fetch_one(self.db.pool())
        .await?;
        let installed_version: i32 = row.try_get("installed_version")?;
        let latest_version: i32 = row.try_get("latest_version")?;
        Ok(SkillListing {
            id,
            external_key: row.try_get("external_key")?,
            title: row.try_get("title")?,
            summary: row.try_get("summary")?,
            curated: false,
            latest_version,
            installed_version: Some(installed_version),
            enabled: row.try_get("enabled")?,
            update_available: installed_version < latest_version,
        })
    }

    pub async fn publish_curated(
        &self,
        deployment_external_key: &str,
        request: PublishSkillRequest,
    ) -> Result<Uuid, SkillError> {
        let deployment_id: Uuid =
            sqlx::query_scalar("SELECT id FROM platform_deployments WHERE external_key=$1")
                .bind(deployment_external_key)
                .fetch_optional(self.db.pool())
                .await?
                .ok_or(SkillError::NotFound)?;
        self.publish(deployment_id, None, request).await
    }

    async fn publish(
        &self,
        deployment_id: Uuid,
        owner: Option<Uuid>,
        request: PublishSkillRequest,
    ) -> Result<Uuid, SkillError> {
        validate(&request)?;
        let mut tx = self.db.pool().begin().await?;
        let existing = sqlx::query(
            "SELECT id, latest_version FROM skill_packages
             WHERE deployment_id=$1 AND external_key=$2
             AND owner_user_context_id IS NOT DISTINCT FROM $3
             FOR UPDATE",
        )
        .bind(deployment_id)
        .bind(&request.external_key)
        .bind(owner)
        .fetch_optional(&mut *tx)
        .await?;
        let (id, version) = if let Some(row) = existing {
            let id: Uuid = row.get("id");
            let version: i32 = row.get::<i32, _>("latest_version") + 1;
            sqlx::query(
                "UPDATE skill_packages SET title=$2, summary=$3, latest_version=$4,
                 state='active', updated_at=now() WHERE id=$1",
            )
            .bind(id)
            .bind(&request.title)
            .bind(&request.summary)
            .bind(version)
            .execute(&mut *tx)
            .await?;
            (id, version)
        } else {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO skill_packages
                 (deployment_id,owner_user_context_id,external_key,title,summary)
                 VALUES ($1,$2,$3,$4,$5) RETURNING id",
            )
            .bind(deployment_id)
            .bind(owner)
            .bind(&request.external_key)
            .bind(&request.title)
            .bind(&request.summary)
            .fetch_one(&mut *tx)
            .await?;
            (id, 1)
        };
        sqlx::query(
            "INSERT INTO skill_package_versions
             (skill_id,version,instructions,requested_capabilities,resources)
             VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(id)
        .bind(version)
        .bind(&request.instructions)
        .bind(&request.requested_capabilities)
        .bind(&request.resources)
        .execute(&mut *tx)
        .await?;
        if let Some(owner) = owner {
            // Existing installations stay pinned until the user reviews the update.
            sqlx::query(
                "INSERT INTO skill_installations (user_context_id,skill_id,installed_version)
                 VALUES ($1,$2,$3) ON CONFLICT (user_context_id,skill_id) DO NOTHING",
            )
            .bind(owner)
            .bind(id)
            .bind(version)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(id)
    }

    pub async fn list(
        &self,
        context: &ResolvedUserContext,
    ) -> Result<Vec<SkillListing>, SkillError> {
        let rows = sqlx::query(
            "SELECT s.id,s.external_key,s.title,s.summary,s.owner_user_context_id,
                    s.latest_version,i.installed_version,COALESCE(i.enabled,false) AS enabled
             FROM skill_packages s LEFT JOIN skill_installations i
               ON i.skill_id=s.id AND i.user_context_id=$2
             WHERE s.deployment_id=$1 AND s.state='active'
               AND (s.owner_user_context_id IS NULL OR s.owner_user_context_id=$2)
             ORDER BY s.title,s.external_key LIMIT 200",
        )
        .bind(context.subject.deployment_id.0)
        .bind(context.id.0)
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let installed_version: Option<i32> = row.try_get("installed_version")?;
                let latest_version: i32 = row.try_get("latest_version")?;
                Ok(SkillListing {
                    id: row.try_get("id")?,
                    external_key: row.try_get("external_key")?,
                    title: row.try_get("title")?,
                    summary: row.try_get("summary")?,
                    curated: row
                        .try_get::<Option<Uuid>, _>("owner_user_context_id")?
                        .is_none(),
                    latest_version,
                    installed_version,
                    enabled: row.try_get("enabled")?,
                    update_available: installed_version.is_some_and(|v| v < latest_version),
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(SkillError::Database)
    }

    pub async fn version(
        &self,
        context: &ResolvedUserContext,
        skill_id: Uuid,
        version: i32,
    ) -> Result<SkillVersion, SkillError> {
        let row = sqlx::query(
            "SELECT v.version,v.instructions,v.requested_capabilities,v.resources
             FROM skill_package_versions v JOIN skill_packages s ON s.id=v.skill_id
             WHERE s.id=$1 AND v.version=$2 AND s.deployment_id=$3
               AND s.state='active'
               AND (s.owner_user_context_id IS NULL OR s.owner_user_context_id=$4)",
        )
        .bind(skill_id)
        .bind(version)
        .bind(context.subject.deployment_id.0)
        .bind(context.id.0)
        .fetch_optional(self.db.pool())
        .await?
        .ok_or(SkillError::NotFound)?;
        Ok(SkillVersion {
            version: row.try_get("version")?,
            instructions: row.try_get("instructions")?,
            requested_capabilities: row.try_get("requested_capabilities")?,
            resources: row.try_get("resources")?,
        })
    }

    pub async fn install(
        &self,
        context: &ResolvedUserContext,
        skill_id: Uuid,
        reviewed_version: i32,
    ) -> Result<(), SkillError> {
        if reviewed_version < 1 {
            return Err(SkillError::Invalid);
        }
        let mut tx = self.db.pool().begin().await?;
        let version: Option<i32> = sqlx::query_scalar(
            "SELECT latest_version FROM skill_packages
             WHERE id=$1 AND deployment_id=$2 AND state='active'
               AND (owner_user_context_id IS NULL OR owner_user_context_id=$3)
             FOR UPDATE",
        )
        .bind(skill_id)
        .bind(context.subject.deployment_id.0)
        .bind(context.id.0)
        .fetch_optional(&mut *tx)
        .await?;
        let version = version.ok_or(SkillError::NotFound)?;
        if version != reviewed_version {
            return Err(SkillError::Conflict);
        }
        sqlx::query(
            "INSERT INTO skill_installations
             (user_context_id,skill_id,installed_version,enabled)
             VALUES ($1,$2,$3,true)
             ON CONFLICT (user_context_id,skill_id) DO UPDATE
             SET installed_version=EXCLUDED.installed_version,enabled=true,updated_at=now()",
        )
        .bind(context.id.0)
        .bind(skill_id)
        .bind(version)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn disable(
        &self,
        context: &ResolvedUserContext,
        skill_id: Uuid,
    ) -> Result<(), SkillError> {
        let changed = sqlx::query(
            "UPDATE skill_installations SET enabled=false,updated_at=now()
             WHERE user_context_id=$1 AND skill_id=$2",
        )
        .bind(context.id.0)
        .bind(skill_id)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if changed == 0 {
            Err(SkillError::NotFound)
        } else {
            Ok(())
        }
    }

    /// Select which agent may load this skill's instructions. This does not
    /// create a capability grant or authorize a provider call.
    pub async fn set_agent_enabled(
        &self,
        context: &ResolvedUserContext,
        agent_key: &str,
        skill_id: Uuid,
        enabled: bool,
    ) -> Result<(), SkillError> {
        let agent_id = self.selected_agent_id(context, agent_key).await?;
        let installed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM skill_installations i
             JOIN skill_packages s ON s.id=i.skill_id
             WHERE i.user_context_id=$1 AND i.skill_id=$2 AND i.enabled=true
               AND s.deployment_id=$3 AND s.state='active'
               AND (s.owner_user_context_id IS NULL OR s.owner_user_context_id=$1))",
        )
        .bind(context.id.0)
        .bind(skill_id)
        .bind(context.subject.deployment_id.0)
        .fetch_one(self.db.pool())
        .await?;
        if !installed {
            return Err(SkillError::NotFound);
        }
        sqlx::query(
            "INSERT INTO skill_agent_enablements
             (user_context_id,skill_id,agent_definition_id,enabled)
             VALUES ($1,$2,$3,$4)
             ON CONFLICT (user_context_id,skill_id,agent_definition_id)
             DO UPDATE SET enabled=EXCLUDED.enabled,updated_at=now()",
        )
        .bind(context.id.0)
        .bind(skill_id)
        .bind(agent_id)
        .bind(enabled)
        .execute(self.db.pool())
        .await?;
        Ok(())
    }

    async fn selected_agent_id(
        &self,
        context: &ResolvedUserContext,
        agent_key: &str,
    ) -> Result<Uuid, SkillError> {
        let selected: Option<Uuid> = sqlx::query_scalar(
            "SELECT a.id FROM agent_definitions a
             JOIN deployment_agent_selections sel ON sel.agent_definition_id=a.id
             WHERE a.deployment_id=$1 AND a.external_key=$2 AND a.state='enabled'",
        )
        .bind(context.subject.deployment_id.0)
        .bind(agent_key)
        .fetch_optional(self.db.pool())
        .await?;
        selected.ok_or(SkillError::NotFound)
    }

    pub async fn effective(
        &self,
        context: &ResolvedUserContext,
        agent_key: &str,
    ) -> Result<Vec<EffectiveSkill>, SkillError> {
        let agent_id = self.selected_agent_id(context, agent_key).await?;
        let granted: Vec<String> = CapabilityGrantService::new(self.db.clone())
            .effective_for_agent(context, agent_key)
            .await?
            .into_iter()
            .map(|grant| grant.capability_external_key)
            .collect();
        let rows = sqlx::query(
            "SELECT s.id,s.external_key,s.title,s.summary,i.installed_version,
                    v.requested_capabilities
             FROM skill_installations i JOIN skill_agent_enablements e
               ON e.user_context_id=i.user_context_id AND e.skill_id=i.skill_id
               AND e.agent_definition_id=$3 AND e.enabled=true
             JOIN skill_packages s ON s.id=i.skill_id
             JOIN skill_package_versions v
               ON v.skill_id=s.id AND v.version=i.installed_version
             WHERE i.user_context_id=$1 AND i.enabled=true AND s.state='active'
               AND s.deployment_id=$2
               AND (s.owner_user_context_id IS NULL OR s.owner_user_context_id=$1)
             ORDER BY s.external_key LIMIT 64",
        )
        .bind(context.id.0)
        .bind(context.subject.deployment_id.0)
        .bind(agent_id)
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let requested: Vec<String> = row.try_get("requested_capabilities")?;
                let available_capabilities = requested
                    .iter()
                    .filter(|key| granted.contains(key))
                    .cloned()
                    .collect();
                Ok(EffectiveSkill {
                    id: row.try_get("id")?,
                    external_key: row.try_get("external_key")?,
                    title: row.try_get("title")?,
                    summary: row.try_get("summary")?,
                    version: row.try_get("installed_version")?,
                    requested_capabilities: requested,
                    available_capabilities,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(SkillError::Database)
    }

    pub async fn load_for_agent(
        &self,
        context: &ResolvedUserContext,
        agent_key: &str,
        skill_id: Uuid,
    ) -> Result<LoadedSkill, SkillError> {
        let selected = self
            .effective(context, agent_key)
            .await?
            .into_iter()
            .find(|skill| skill.id == skill_id)
            .ok_or(SkillError::NotFound)?;
        let version = self.version(context, skill_id, selected.version).await?;
        Ok(LoadedSkill {
            id: selected.id,
            external_key: selected.external_key,
            title: selected.title,
            version: selected.version,
            instructions: version.instructions,
            resources: version.resources,
            available_capabilities: selected.available_capabilities,
        })
    }
}

fn validate(request: &PublishSkillRequest) -> Result<(), SkillError> {
    let key = request.external_key.as_str();
    if key.is_empty()
        || key.len() > 128
        || !key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(SkillError::Invalid);
    }
    if request.title.trim().is_empty()
        || request.title.len() > 120
        || request.summary.trim().is_empty()
        || request.summary.len() > 500
        || request.instructions.trim().is_empty()
        || request.instructions.len() > 16_384
        || request.requested_capabilities.len() > 64
        || request
            .requested_capabilities
            .iter()
            .any(|v| v.is_empty() || v.len() > 255)
        || !request.resources.is_object()
        || serde_json::to_vec(&request.resources).map_or(true, |v| v.len() > 32_768)
        || contains_secret(&request.instructions)
        || contains_secret(&request.resources.to_string())
    {
        return Err(SkillError::Invalid);
    }
    Ok(())
}

fn empty_resources() -> Value {
    serde_json::json!({})
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "-----begin private key",
        "-----begin rsa private key",
        "bearer ",
        "api_key=",
        "client_secret=",
        "sk-proj-",
        "sk_live_",
        "ghp_",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> PublishSkillRequest {
        PublishSkillRequest {
            external_key: "meeting-prep".into(),
            title: "Meeting prep".into(),
            summary: "Prepare a concise agenda".into(),
            instructions: "Summarize the purpose and ask for missing agenda items.".into(),
            requested_capabilities: vec!["calendar.read".into()],
            resources: empty_resources(),
        }
    }

    #[test]
    fn skill_declarations_are_bounded_and_contain_no_obvious_credentials() {
        assert!(validate(&valid()).is_ok());
        let mut invalid = valid();
        invalid.instructions = "Use Bearer abc123 to connect".into();
        assert!(matches!(validate(&invalid), Err(SkillError::Invalid)));
        let mut invalid = valid();
        invalid.resources = serde_json::json!({"token": "sk_live_example"});
        assert!(matches!(validate(&invalid), Err(SkillError::Invalid)));
        let mut invalid = valid();
        invalid.external_key = "../meeting".into();
        assert!(matches!(validate(&invalid), Err(SkillError::Invalid)));
    }
}
