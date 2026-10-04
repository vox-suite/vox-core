use sqlx::Row;
const MAX_CAPABILITY_KEY_BYTES: usize = 511;
use crate::identity_contract::RequestContext;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CapabilityGrant {
    pub id: Uuid,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub capability_external_key: String,
}
#[derive(Clone)]
pub struct CapabilityGrantService {
    db: sqlx::PgPool,
}
#[derive(Debug, thiserror::Error)]
pub enum CapabilityGrantError {
    #[error("Invalid grant reference")]
    Invalid,
    #[error("Connector capability grants have been retired")]
    Unavailable,
    #[error("Grant storage unavailable")]
    Database(#[from] sqlx::Error),
}
impl CapabilityGrantService {
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { db: pool }
    }
    pub async fn effective_for_agent(
        &self,
        context: &RequestContext,
        agent_external_key: &str,
    ) -> Result<Vec<CapabilityGrant>, CapabilityGrantError> {
        self.effective(context, agent_external_key, None, None)
            .await
    }

    pub async fn effective_for_tool(
        &self,
        context: &RequestContext,
        agent: &str,
        connection: Uuid,
        capability: &str,
    ) -> Result<Option<CapabilityGrant>, CapabilityGrantError> {
        let capability = key(capability)?;
        Ok(self
            .effective(context, agent, Some(connection), Some(&capability))
            .await?
            .into_iter()
            .next())
    }

    async fn effective(
        &self,
        context: &RequestContext,
        agent_external_key: &str,
        connection: Option<Uuid>,
        capability: Option<&str>,
    ) -> Result<Vec<CapabilityGrant>, CapabilityGrantError> {
        let agent_key = key(agent_external_key)?;
        let rows = sqlx::query(
            "SELECT g.id, a.external_key, g.connection_id, g.capability_external_key \
             FROM retired_agent_capability_grants g \
             JOIN agent_definitions a ON a.id = g.agent_definition_id \
             JOIN deployment_agent_selections s ON s.agent_definition_id = a.id \
             JOIN retired_external_connections x ON x.id = g.connection_id \
             WHERE g.user_context_id = $1 AND a.deployment_id = $2 AND a.external_key = $3 \
             AND ($4::uuid IS NULL OR g.connection_id=$4) AND ($5::text IS NULL OR g.capability_external_key=$5) \
             AND a.state = 'enabled' AND (a.template_id IS NULL OR EXISTS (SELECT 1 FROM agent_definitions template WHERE template.id=a.template_id AND template.state='enabled' AND (g.capability_external_key=ANY(template.requested_capability_categories) OR '*'=ANY(template.requested_capability_categories)))) AND a.owner_user_context_id = $1 AND g.state = 'enabled' \
             AND x.user_context_id = $1 AND x.authorization_state = 'authorized' \
             AND (x.expires_at IS NULL OR x.expires_at > now()) \
             AND (g.capability_external_key = ANY(a.requested_capability_categories) \
                  OR '*' = ANY(a.requested_capability_categories)) \
             AND g.capability_external_key = ANY(x.authorized_capabilities) \
             AND (EXISTS (SELECT 1 FROM retired_integration_definitions i \
                  JOIN integration_capability_declarations c ON c.integration_id=i.id \
                  WHERE i.id=x.integration_id AND i.deployment_id=$2 AND i.state='enabled' \
                  AND concat(i.external_key,'.',c.external_key)=g.capability_external_key) \
               OR EXISTS (SELECT 1 FROM retired_remote_extensions e \
                  JOIN retired_remote_extension_versions v ON v.extension_id=e.id AND v.version=e.current_version \
                  WHERE e.id=x.remote_extension_id AND e.user_context_id=x.user_context_id \
                  AND e.lifecycle_state='active' AND e.consent_status='consented' \
                  AND EXISTS (SELECT 1 FROM jsonb_array_elements(v.capabilities) cap \
                              WHERE cap->>'external_key'=g.capability_external_key))) \
             ORDER BY g.capability_external_key",
        )
        .bind(context.id.0)
        .bind(context.subject.deployment_id.0)
        .bind(agent_key)
        .bind(connection)
        .bind(capability)
        .fetch_all(&self.db)
        .await?;
        rows.into_iter()
            .map(|row| {
                Ok(CapabilityGrant {
                    id: row.try_get("id")?,
                    agent_external_key: row.try_get("external_key")?,
                    connection_id: row.try_get("connection_id")?,
                    capability_external_key: row.try_get("capability_external_key")?,
                })
            })
            .collect()
    }
}
fn key(value: &str) -> Result<String, CapabilityGrantError> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_CAPABILITY_KEY_BYTES {
        Err(CapabilityGrantError::Invalid)
    } else {
        Ok(value.to_owned())
    }
}
