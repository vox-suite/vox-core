use crate::identity::ResourceOwner;
use sqlx::PgPool;
use std::collections::HashSet;
use uuid::Uuid;

/// Resolve the actual account declaration, not the normalized timeline source.
/// Both grant state and account preferences are checked on every read.
pub(crate) async fn permitted_connections(
    pool: &PgPool,
    owner: ResourceOwner,
    agent: Option<&str>,
) -> Result<HashSet<Uuid>, sqlx::Error> {
    use vox_connections::identity::{
        DeploymentId, RequestContext, RequestSubject, UserContextId, UserId,
    };
    let deployment: Uuid =
        sqlx::query_scalar("SELECT deployment_id FROM user_contexts WHERE id=$1 AND user_id=$2")
            .bind(owner.user_context_id.0)
            .bind(owner.user_id.0)
            .fetch_one(pool)
            .await?;
    let Some(agent) = agent else {
        return Ok(HashSet::new());
    };
    let scope = RequestContext {
        id: UserContextId(owner.user_context_id.0),
        user_id: UserId(owner.user_id.0),
        subject: RequestSubject {
            deployment_id: DeploymentId(deployment),
        },
    };
    let grants = vox_connections::capability_grants::CapabilityGrantService::new(pool.clone())
        .effective_for_agent(&scope, agent)
        .await
        .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    let accounts: Vec<(Uuid, String, bool)> = sqlx::query_as("SELECT id,connector_id,(authorization_state='authorized' AND consented_at IS NOT NULL AND assistant_read AND sync_timeline) FROM vox_connections WHERE user_context_id=$1 AND user_id=$2")
        .bind(owner.user_context_id.0).bind(owner.user_id.0).fetch_all(pool).await?;
    let accounts: std::collections::HashMap<_, _> = accounts
        .into_iter()
        .map(|(id, key, enabled)| (id, (key, enabled)))
        .collect();
    Ok(grants
        .into_iter()
        .filter(|g| match accounts.get(&g.connection_id) {
            Some((key, enabled)) => {
                *enabled && g.capability_external_key == format!("curated_{key}.read")
            }
            None => g.capability_external_key == "playstation.game_activity",
        })
        .map(|g| g.connection_id)
        .collect())
}
