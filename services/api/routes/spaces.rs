use std::sync::Arc;

use axum::{
    Extension, Json,
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

use vox_core::{
    agents::space_runtime::SpaceRuntime,
    domain::{
        collections::CollectionKind,
        identity::Actor,
        spaces::{NodeState, Space, SpaceGraph, SpaceMessage, SpaceNode, SpaceState},
    },
    realtime::UserEventHub,
    storage::spaces::SpaceRepository,
};

#[derive(Clone)]
pub struct SpaceApiState {
    pub pool: PgPool,
    pub spaces: SpaceRepository,
    pub runtime: Arc<SpaceRuntime>,
    pub user_events: UserEventHub,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct CreateSpaceInput {
    #[serde(default)]
    pub title: Option<String>,
    pub intent: String,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct SendSpaceChatInput {
    pub message: String,
    #[serde(default)]
    pub node_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::ToSchema)]
pub struct UpdateSpaceNodeInput {
    pub state: Option<String>,
    pub title: Option<String>,
    pub body: Option<String>,
    pub position: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, Deserialize, utoipa::ToSchema)]
pub struct CommitSpaceResult {
    pub collection_id: Uuid,
    pub committed_spans_count: usize,
}

#[utoipa::path(
    post,
    path = "/v1/me/spaces",
    tag = "spaces",
    request_body = crate::routes::spaces::CreateSpaceInput,
    responses((status = 201, body = vox_core::domain::spaces::Space))
)]
pub async fn create_space(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateSpaceInput>,
) -> Result<impl IntoResponse, StatusCode> {
    let intent = input.intent.trim();
    if intent.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let title = input
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .unwrap_or("New space");
    let space = state
        .spaces
        .create_workflow_space(actor.user_id, title, intent)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let runtime = state.runtime.clone();
    let space_id = space.id;
    tokio::spawn(async move {
        let _ = runtime.run_space(space_id, None).await;
    });

    state.user_events.notify(
        actor.user_id,
        json!({
            "type": "space_created",
            "space": space
        }),
    );

    Ok((StatusCode::CREATED, Json(space)))
}

#[utoipa::path(
    get,
    path = "/v1/me/spaces",
    tag = "spaces",
    responses((status = 200, body = Vec<vox_core::domain::spaces::Space>))
)]
pub async fn list_spaces(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<Space>>, StatusCode> {
    let spaces = state
        .spaces
        .list_spaces(actor.user_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(spaces))
}

#[utoipa::path(
    get,
    path = "/v1/me/spaces/{id}",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = vox_core::domain::spaces::SpaceGraph))
)]
pub async fn get_space(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<SpaceGraph>, StatusCode> {
    let graph = state
        .spaces
        .get_graph(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match graph {
        Some(g) => Ok(Json(g)),
        None => Err(StatusCode::NOT_FOUND),
    }
}

#[utoipa::path(
    delete,
    path = "/v1/me/spaces/{id}",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 204))
)]
pub async fn drop_space(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let dropped = state
        .spaces
        .drop_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if dropped {
        state.user_events.notify(
            actor.user_id,
            json!({
                "type": "space_dropped",
                "space_id": id
            }),
        );
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(StatusCode::NOT_FOUND)
    }
}

#[utoipa::path(
    post,
    path = "/v1/me/spaces/{id}/chat",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path)),
    request_body = crate::routes::spaces::SendSpaceChatInput,
    responses((status = 200, body = serde_json::Value))
)]
pub async fn send_space_chat(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<SendSpaceChatInput>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let message = input.message.trim();
    if message.is_empty() {
        return Err(StatusCode::BAD_REQUEST);
    }

    let existing = state
        .spaces
        .get_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    match existing {
        None => return Err(StatusCode::NOT_FOUND),
        Some(space) if matches!(space.state, SpaceState::Committed | SpaceState::Dropped) => {
            return Err(StatusCode::CONFLICT);
        }
        Some(_) => {}
    }

    let context = if let Some(node_id) = input.node_id {
        let graph = state
            .spaces
            .get_graph(actor.user_id, id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        let node = graph
            .nodes
            .iter()
            .find(|n| n.id == node_id)
            .ok_or(StatusCode::NOT_FOUND)?;
        format!(
            "Selected node {} ({}): {}\nUser instruction: {}",
            node.id, node.title, node.body, message
        )
    } else {
        message.to_string()
    };
    let workflow = state
        .spaces
        .get_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .is_some_and(|s| s.agent_spec["workflow_version"] == 2);
    if workflow {
        let mut tx = state
            .pool
            .begin()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let current =
            sqlx::query("SELECT state,workflow_generation FROM spaces WHERE id=$1 FOR UPDATE")
                .bind(id)
                .fetch_one(&mut *tx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if matches!(
            current.get::<String, _>("state").as_str(),
            "committed" | "dropped"
        ) {
            return Err(StatusCode::CONFLICT);
        }
        let generation: i32 = current.get("workflow_generation");
        sqlx::query(
            "INSERT INTO space_workflow_requests(space_id,message,node_id,generation) VALUES($1,$2,$3,$4)",
        )
        .bind(id)
        .bind(message)
        .bind(input.node_id)
        .bind(generation)
        .execute(&mut *tx)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        sqlx::query("INSERT INTO space_messages(space_id,role,text) VALUES($1,'user',$2)")
            .bind(id)
            .bind(message)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        sqlx::query("INSERT INTO jobs(kind,payload_reference_id) VALUES('run_space',$1)")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        tx.commit()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    let runtime = state.runtime.clone();
    tokio::spawn(async move {
        let _ = runtime
            .run_space(id, if workflow { None } else { Some(context) })
            .await;
    });
    state.user_events.notify(
        actor.user_id,
        json!({"type":"space_message_created","space_id":id}),
    );

    Ok(Json(json!({ "status": "processing" })))
}

#[utoipa::path(
    post,
    path = "/v1/me/spaces/{id}/commit",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = crate::routes::spaces::CommitSpaceResult))
)]
pub async fn commit_space(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<CommitSpaceResult>, StatusCode> {
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let space_row = sqlx::query(
        "SELECT id, user_id, title, intent, state, agent_spec, committed_collection_id FROM spaces WHERE user_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(actor.user_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(space_row) = space_row else {
        return Err(StatusCode::NOT_FOUND);
    };

    let current_state: String = space_row.get("state");
    if current_state == "committed" || current_state == "dropped" {
        return Err(StatusCode::CONFLICT);
    }

    let spec: serde_json::Value = space_row.get("agent_spec");
    if spec["workflow_version"] == 2 {
        let ready:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM space_tasks WHERE space_id=$1 AND role='plan' AND status='done') AND NOT EXISTS(SELECT 1 FROM space_tasks WHERE space_id=$1 AND (status!='done' OR NOT expanded)) AND NOT EXISTS(SELECT 1 FROM space_workflow_requests WHERE space_id=$1 AND NOT processed)").bind(id).fetch_one(&mut *tx).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
        if !ready {
            return Err(StatusCode::CONFLICT);
        }
    }
    let unfinished: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM space_tasks WHERE space_id=$1 AND status != 'done'",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if unfinished > 0 {
        return Err(StatusCode::CONFLICT);
    }

    let title: String = space_row.get("title");
    let intent: String = space_row.get("intent");

    let kind = if intent.to_lowercase().contains("trip") || title.to_lowercase().contains("trip") {
        CollectionKind::Trip
    } else {
        CollectionKind::Custom
    };

    let collection_id = sqlx::query_scalar::<_, Uuid>(
        "INSERT INTO collections (user_id, name, description, kind, metadata)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(actor.user_id)
    .bind(title.trim())
    .bind(intent.trim())
    .bind(kind.as_str())
    .bind(json!({ "space_id": id }))
    .fetch_one(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let node_rows = sqlx::query(
        "SELECT id, kind, title, body, data FROM space_nodes WHERE space_id = $1 AND state = 'done' ORDER BY created_at ASC",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let mut committed_spans_count = 0;
    for node in &node_rows {
        let node_kind: String = node.get("kind");
        if matches!(node_kind.as_str(), "plan" | "step") {
            let node_title: String = node.get("title");
            let node_body: String = node.get("body");
            let node_data: serde_json::Value = node.get("data");

            let span_id = sqlx::query_scalar::<_, Uuid>(
                r#"
                INSERT INTO spans (user_id, title, notes, status, data, category, source)
                VALUES ($1, $2, $3, 'planned', $4, 'general', 'space')
                RETURNING id
                "#,
            )
            .bind(actor.user_id)
            .bind(&node_title)
            .bind(&node_body)
            .bind(&node_data)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            sqlx::query(
                "INSERT INTO collection_spans (collection_id, span_id, user_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING",
            )
            .bind(collection_id)
            .bind(span_id)
            .bind(actor.user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

            committed_spans_count += 1;
        }
    }

    sqlx::query(
        "UPDATE spaces SET state = 'committed', committed_collection_id = $1, updated_at = now() WHERE id = $2",
    )
    .bind(collection_id)
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    tx.commit()
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    state.user_events.notify(
        actor.user_id,
        json!({
            "type": "space_committed",
            "space_id": id,
            "collection_id": collection_id
        }),
    );

    Ok(Json(CommitSpaceResult {
        collection_id,
        committed_spans_count,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/me/spaces/{id}/messages",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path)),
    responses((status = 200, body = Vec<vox_core::domain::spaces::SpaceMessage>))
)]
pub async fn list_space_messages(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<SpaceMessage>>, StatusCode> {
    let existing = state
        .spaces
        .get_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if existing.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    let messages = state
        .spaces
        .list_messages(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    Ok(Json(messages))
}

#[utoipa::path(
    patch,
    path = "/v1/me/spaces/{id}/nodes/{node_id}",
    tag = "spaces",
    params(("id" = uuid::Uuid, Path), ("node_id" = uuid::Uuid, Path)),
    request_body = crate::routes::spaces::UpdateSpaceNodeInput,
    responses((status = 200, body = vox_core::domain::spaces::SpaceNode))
)]
pub async fn update_space_node(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path((id, node_id)): Path<(Uuid, Uuid)>,
    Json(input): Json<UpdateSpaceNodeInput>,
) -> Result<Json<SpaceNode>, StatusCode> {
    let parsed_state = match input.state.as_deref() {
        Some("done") => Some(NodeState::Done),
        Some("rejected") => Some(NodeState::Rejected),
        Some(_) => return Err(StatusCode::BAD_REQUEST),
        None => None,
    };

    let existing_space = state
        .spaces
        .get_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(space) = existing_space else {
        return Err(StatusCode::NOT_FOUND);
    };

    if matches!(space.state, SpaceState::Committed | SpaceState::Dropped) {
        return Err(StatusCode::CONFLICT);
    }

    let existing_node = state
        .spaces
        .get_node(id, node_id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    if existing_node.is_none() {
        return Err(StatusCode::NOT_FOUND);
    }

    if space.agent_spec["workflow_version"] == 2 {
        let mut tx = state
            .pool
            .begin()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let current: String = sqlx::query_scalar("SELECT state FROM spaces WHERE id=$1 FOR UPDATE")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if matches!(current.as_str(), "committed" | "dropped") {
            return Err(StatusCode::CONFLICT);
        }
        sqlx::query("UPDATE space_nodes SET title=coalesce($3,title),body=coalesce($4,body),position=coalesce($5,position),state=coalesce($6,state),version=version+1,updated_at=now() WHERE space_id=$1 AND id=$2").bind(id).bind(node_id).bind(&input.title).bind(&input.body).bind(&input.position).bind(parsed_state.map(NodeState::as_str)).execute(&mut *tx).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
        if input.title.is_some()
            || input.body.is_some()
            || parsed_state == Some(NodeState::Rejected)
        {
            sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT e.to_node FROM space_edges e WHERE e.space_id=$1 AND e.from_node=$2 UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) UPDATE space_tasks SET status='cancelled',error='Prerequisite changed; retry this branch',lease_token=NULL,expanded=true WHERE space_id=$1 AND node_id IN(SELECT id FROM descendants)").bind(id).bind(node_id).execute(&mut *tx).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
            sqlx::query("UPDATE space_tasks SET status=$3,brief=coalesce($4,brief),lease_token=NULL,expanded=false,attempt=0,error=NULL WHERE space_id=$1 AND node_id=$2").bind(id).bind(node_id).bind(if parsed_state==Some(NodeState::Rejected){"cancelled"}else{"queued"}).bind(&input.body).execute(&mut *tx).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
            sqlx::query("INSERT INTO jobs(kind,payload_reference_id) VALUES('run_space',$1)")
                .bind(id)
                .execute(&mut *tx)
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }
        tx.commit()
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        vox_core::storage::space_tasks::TaskRepository::new(state.pool.clone())
            .sync_metadata(id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        let node = state
            .spaces
            .get_node(id, node_id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
            .ok_or(StatusCode::NOT_FOUND)?;
        state.user_events.notify(
            actor.user_id,
            json!({"type":"space_node_updated","space_id":id,"node":node}),
        );
        return Ok(Json(node));
    }
    let updated = state
        .spaces
        .update_node_scoped(
            actor.user_id,
            id,
            node_id,
            input.title.as_deref(),
            input.body.as_deref(),
            None,
            parsed_state,
            input.position,
            None,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

    let Some(node) = updated else {
        return Err(StatusCode::NOT_FOUND);
    };

    if space.agent_spec["workflow_version"] == 2
        && (input.title.is_some()
            || input.body.is_some()
            || parsed_state == Some(NodeState::Rejected))
    {
        sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT e.to_node FROM space_edges e WHERE e.space_id=$1 AND e.from_node=$2 UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) UPDATE space_tasks SET status='cancelled',error='Prerequisite changed; retry this branch',lease_token=NULL,expanded=false WHERE space_id=$1 AND node_id IN(SELECT id FROM descendants)").bind(id).bind(node_id).execute(&state.pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
        sqlx::query("UPDATE space_tasks SET status=$3,lease_token=NULL,expanded=true WHERE space_id=$1 AND node_id=$2").bind(id).bind(node_id).bind(if parsed_state==Some(NodeState::Rejected){"cancelled"}else{"done"}).execute(&state.pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
        vox_core::storage::space_tasks::TaskRepository::new(state.pool.clone())
            .sync_metadata(id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    if parsed_state == Some(NodeState::Rejected) || input.title.is_some() || input.body.is_some() {
        let stale = state
            .spaces
            .mark_descendants_stale(id, node.id)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;

        for stale_id in stale {
            if let Ok(Some(n)) = state.spaces.get_node(id, stale_id).await {
                state.user_events.notify(
                    actor.user_id,
                    json!({
                        "type": "space_node_updated",
                        "space_id": id,
                        "node": n
                    }),
                );
            }
        }
    }

    state.user_events.notify(
        actor.user_id,
        json!({
            "type": "space_node_updated",
            "space_id": id,
            "node": node
        }),
    );

    Ok(Json(node))
}

#[utoipa::path(post,path="/v1/me/spaces/{id}/stop",tag="spaces",params(("id"=Uuid,Path)),responses((status=200)))]
pub async fn stop_space(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    state
        .spaces
        .get_space(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    vox_core::storage::space_tasks::TaskRepository::new(state.pool)
        .stop(id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    state.user_events.notify(
        actor.user_id,
        json!({"type":"space_graph_updated","space_id":id}),
    );
    Ok(Json(json!({"status":"stopped"})))
}
#[utoipa::path(post,path="/v1/me/spaces/{id}/nodes/{node}/retry",tag="spaces",params(("id"=Uuid,Path),("node"=Uuid,Path)),responses((status=200)))]
pub async fn retry_space_node(
    State(state): State<SpaceApiState>,
    Extension(actor): Extension<Actor>,
    Path((id, node)): Path<(Uuid, Uuid)>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    let graph = state
        .spaces
        .get_graph(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if matches!(
        graph.space.state,
        SpaceState::Committed | SpaceState::Dropped
    ) {
        return Err(StatusCode::CONFLICT);
    }
    if !graph.nodes.iter().any(|n| n.id == node) {
        return Err(StatusCode::NOT_FOUND);
    }
    vox_core::storage::space_tasks::TaskRepository::new(state.pool)
        .retry(id, node)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let runtime = state.runtime;
    tokio::spawn(async move {
        let _ = runtime.run_space(id, None).await;
    });
    Ok(Json(json!({"status":"queued"})))
}
