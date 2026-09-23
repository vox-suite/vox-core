use crate::{db::Db, identity::ResolvedUserContext};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use uuid::Uuid;

const MAX_CAPABILITY_KEY_BYTES: usize = 511;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CreateGrantRequest {
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub capability_external_key: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapabilityGrant {
    pub id: Uuid,
    pub agent_external_key: String,
    pub connection_id: Uuid,
    pub capability_external_key: String,
}

#[derive(Clone)]
pub struct CapabilityGrantService {
    db: Db,
}

#[derive(Debug, thiserror::Error)]
pub enum CapabilityGrantError {
    #[error("capability grant request is invalid")]
    Invalid,
    #[error("agent, connection, or capability is unavailable")]
    Unavailable,
    #[error("capability grant storage is unavailable")]
    Database(#[from] sqlx::Error),
}

impl CapabilityGrantService {
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    pub async fn grant(
        &self,
        context: &ResolvedUserContext,
        request: CreateGrantRequest,
    ) -> Result<CapabilityGrant, CapabilityGrantError> {
        let agent_key = key(&request.agent_external_key)?;
        let capability_key = key(&request.capability_external_key)?;
        let changed = sqlx::query(
            "UPDATE connections SET allowed_capabilities = array_append(allowed_capabilities, $3), updated_at = now() \
             WHERE id = $1 AND user_id = $2 AND authorization_state = 'authorized' \
             AND (expires_at IS NULL OR expires_at > now()) \
             AND NOT ($3 = ANY(allowed_capabilities))",
        )
        .bind(request.connection_id)
        .bind(context.user_id.0)
        .bind(&capability_key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if changed == 0 {
            let exists = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM connections WHERE id = $1 AND user_id = $2 \
                 AND authorization_state = 'authorized' AND (expires_at IS NULL OR expires_at > now()) \
                 AND $3 = ANY(allowed_capabilities))",
            )
            .bind(request.connection_id)
            .bind(context.user_id.0)
            .bind(&capability_key)
            .fetch_one(self.db.pool())
            .await?;
            if !exists {
                return Err(CapabilityGrantError::Unavailable);
            }
        }
        Ok(CapabilityGrant {
            id: Uuid::new_v4(),
            agent_external_key: agent_key,
            connection_id: request.connection_id,
            capability_external_key: capability_key,
        })
    }

    pub async fn effective_for_agent(
        &self,
        context: &ResolvedUserContext,
        agent_external_key: &str,
    ) -> Result<Vec<CapabilityGrant>, CapabilityGrantError> {
        let agent_key = key(agent_external_key)?;
        let rows = sqlx::query(
            "SELECT id, unnest(allowed_capabilities) AS capability_external_key \
             FROM connections \
             WHERE user_id = $1 AND authorization_state = 'authorized' \
             AND (expires_at IS NULL OR expires_at > now()) \
             AND cardinality(allowed_capabilities) > 0 \
             ORDER BY capability_external_key",
        )
        .bind(context.user_id.0)
        .fetch_all(self.db.pool())
        .await?;
        rows.into_iter()
            .map(|row| {
                let connection_id: Uuid = row.try_get("id")?;
                Ok(CapabilityGrant {
                    id: Uuid::new_v4(),
                    agent_external_key: agent_key.clone(),
                    connection_id,
                    capability_external_key: row.try_get("capability_external_key")?,
                })
            })
            .collect()
    }

    pub async fn revoke(
        &self,
        context: &ResolvedUserContext,
        request: CreateGrantRequest,
    ) -> Result<(), CapabilityGrantError> {
        let _agent_key = key(&request.agent_external_key)?;
        let capability_key = key(&request.capability_external_key)?;
        let changed = sqlx::query(
            "UPDATE connections SET allowed_capabilities = array_remove(allowed_capabilities, $3), updated_at = now() \
             WHERE id = $1 AND user_id = $2 AND $3 = ANY(allowed_capabilities)",
        )
        .bind(request.connection_id)
        .bind(context.user_id.0)
        .bind(capability_key)
        .execute(self.db.pool())
        .await?
        .rows_affected();
        if changed == 0 {
            Err(CapabilityGrantError::Unavailable)
        } else {
            Ok(())
        }
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
