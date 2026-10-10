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
        _connection: Option<Uuid>,
        _capability: Option<&str>,
    ) -> Result<Vec<CapabilityGrant>, CapabilityGrantError> {
        let agent_key = key(agent_external_key)?;
        let available: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM agent_definitions a JOIN user_contexts u ON u.id=a.owner_user_context_id WHERE a.owner_user_context_id=$1 AND a.deployment_id=$2 AND a.external_key=$3 AND a.state='enabled' AND u.user_id=$4)",
        )
        .bind(context.id.0)
        .bind(context.subject.deployment_id.0)
        .bind(agent_key)
        .bind(context.user_id.0)
        .fetch_one(&self.db)
        .await?;
        if !available {
            return Err(CapabilityGrantError::Unavailable);
        }
        Ok(Vec::new())
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
