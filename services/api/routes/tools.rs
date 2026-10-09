use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use vox_core::{
    agents::tools::export::{ToolExport, ToolInvocation, ToolManifestEntry},
    conversations::service::ConversationService,
    domain::identity::Actor,
    identity::ResolvedUserContext,
};

#[derive(Clone)]
pub struct ToolsApiState {
    pub conversations: Option<Arc<ConversationService>>,
    pub export: ToolExport,
}

#[derive(Deserialize)]
pub struct InvokeBody {
    #[serde(default)]
    pub arguments: Value,
}

async fn context(state: &ToolsApiState, actor: &Actor) -> Result<ResolvedUserContext, StatusCode> {
    let conversations = state
        .conversations
        .as_ref()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    conversations
        .resolve_context_for_user(actor.user_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
}

pub async fn list_tools(
    State(state): State<ToolsApiState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<ToolManifestEntry>>, StatusCode> {
    let ctx = context(&state, &actor).await?;
    Ok(Json(state.export.manifest(&ctx)))
}

pub async fn invoke_tool(
    State(state): State<ToolsApiState>,
    Extension(actor): Extension<Actor>,
    Path(name): Path<String>,
    Json(body): Json<InvokeBody>,
) -> Result<Json<ToolInvocation>, StatusCode> {
    let ctx = context(&state, &actor).await?;
    state
        .export
        .invoke(&ctx, &name, body.arguments)
        .await
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
