use crate::state::ApiState;
use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::Row;
use uuid::Uuid;
use vox_core::{
    domain::{identity::Actor, projects::*},
    storage::projects::ProjectRepository,
};

#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateProjectInput {
    pub name: String,
    pub data: ProjectData,
}
#[derive(Deserialize, utoipa::ToSchema)]
pub struct UpdateProjectInput {
    pub name: String,
    pub data: ProjectData,
    pub revision: i64,
}
#[derive(Deserialize)]
pub struct ProjectListQuery {
    pub before: Option<Uuid>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectList {
    pub items: Vec<Project>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Serialize, utoipa::ToSchema)]
pub struct ProjectCount {
    pub count: i64,
    pub rev: String,
}
#[derive(Deserialize, utoipa::ToSchema)]
pub struct ProjectChatInput {
    pub message: String,
    pub revision: i64,
    #[serde(default)]
    pub excerpts: Vec<ProjectExcerpt>,
}
#[derive(Deserialize, utoipa::ToSchema)]
pub struct ProjectExcerpt {
    pub citation: ProjectCitation,
    pub text: String,
}

fn notify(state: &ApiState, user: Uuid, id: Uuid) {
    state
        .user_events
        .notify(user, json!({"type":"project_updated","project_id":id}));
}
fn valid(name: &str, data: &ProjectData) -> bool {
    !name.trim().is_empty()
        && name.trim().chars().count() <= 160
        && data.validate()
        && serde_json::to_vec(data).is_ok_and(|d| d.len() <= 1500000)
}

#[utoipa::path(get, path="/v1/me/projects/count", tag="projects", responses((status=200, body=ProjectCount)))]
pub async fn count(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<ProjectCount>, StatusCode> {
    let row=sqlx::query("SELECT count(*)::bigint AS count, coalesce(md5(string_agg(id::text||':'||revision::text,',' ORDER BY id)),'empty') AS rev FROM projects WHERE user_id=$1 AND NOT archived").bind(actor.user_id).fetch_one(&state.pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?;
    let count: i64 = row.get("count");
    let rev: String = row.get("rev");
    Ok(Json(ProjectCount {
        count,
        rev: format!("{count}-{rev}"),
    }))
}
#[utoipa::path(get, path="/v1/me/projects", tag="projects", params(("before"=Option<Uuid>, Query)), responses((status=200, body=ProjectList)))]
pub async fn list(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Query(q): Query<ProjectListQuery>,
) -> Result<Json<ProjectList>, StatusCode> {
    let mut items = ProjectRepository(state.pool)
        .list(actor.user_id, q.before)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let next_cursor = if items.len() == 50 {
        items.last().map(|p| p.id)
    } else {
        None
    };
    for p in &mut items {
        p.data.nodes.clear();
        p.data.messages.clear();
        p.data.activity.clear();
    }
    Ok(Json(ProjectList { items, next_cursor }))
}
#[utoipa::path(post, path="/v1/me/projects", tag="projects", request_body=CreateProjectInput, responses((status=201, body=Project)))]
pub async fn create(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<CreateProjectInput>,
) -> Result<(StatusCode, Json<Project>), StatusCode> {
    if !valid(&input.name, &input.data) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let p = ProjectRepository(state.pool.clone())
        .create(actor.user_id, input.name.trim(), &input.data)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    notify(&state, actor.user_id, p.id);
    Ok((StatusCode::CREATED, Json(p)))
}
#[utoipa::path(get, path="/v1/me/projects/{id}", tag="projects", params(("id"=Uuid,Path)), responses((status=200, body=Project)))]
pub async fn get(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<Project>, StatusCode> {
    ProjectRepository(state.pool)
        .get(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .ok_or(StatusCode::NOT_FOUND)
}
#[utoipa::path(put, path="/v1/me/projects/{id}", tag="projects", params(("id"=Uuid,Path)), request_body=UpdateProjectInput, responses((status=200, body=Project),(status=409)))]
pub async fn update(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<UpdateProjectInput>,
) -> Result<Json<Project>, StatusCode> {
    if !valid(&input.name, &input.data) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let p = ProjectRepository(state.pool.clone())
        .update(
            actor.user_id,
            id,
            input.revision,
            input.name.trim(),
            &input.data,
        )
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::CONFLICT)?;
    notify(&state, actor.user_id, id);
    Ok(Json(p))
}
#[utoipa::path(delete, path="/v1/me/projects/{id}", tag="projects", params(("id"=Uuid,Path)), responses((status=204)))]
pub async fn archive(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, StatusCode> {
    let changed=sqlx::query("UPDATE projects SET archived=true,revision=revision+1,updated_at=now() WHERE id=$1 AND user_id=$2 AND NOT archived").bind(id).bind(actor.user_id).execute(&state.pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?.rows_affected();
    if changed == 0 {
        return Err(StatusCode::NOT_FOUND);
    }
    notify(&state, actor.user_id, id);
    Ok(StatusCode::NO_CONTENT)
}
#[utoipa::path(post, path="/v1/me/projects/{id}/chat", tag="projects", params(("id"=Uuid,Path)), request_body=ProjectChatInput, responses((status=200, body=Project)))]
pub async fn chat(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(input): Json<ProjectChatInput>,
) -> Result<Json<Project>, StatusCode> {
    if input.message.trim().is_empty()
        || input.message.len() > 12000
        || input.excerpts.len() > 32
        || input
            .excerpts
            .iter()
            .map(|e| e.text.chars().count())
            .sum::<usize>()
            > 96000
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let repo = ProjectRepository(state.pool.clone());
    let mut p = repo
        .get(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    if p.revision != input.revision {
        return Err(StatusCode::CONFLICT);
    }
    let mut allowed = p
        .data
        .nodes
        .iter()
        .filter(|n| !n.stale)
        .flat_map(|n| n.citations.clone())
        .collect::<Vec<_>>();
    for e in &input.excerpts {
        if !p.data.sources.iter().any(|s| s.id == e.citation.source_id)
            || e.citation.path.starts_with('/')
            || e.citation.path.contains("..")
            || e.citation.path.len() > 1000
            || e.citation.start_line == 0
            || e.citation.end_line < e.citation.start_line
            || e.citation.hash.len() > 128
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        allowed.push(e.citation.clone());
    }
    let excerpts = input
        .excerpts
        .iter()
        .map(|e| {
            format!(
                "{}:{}-{}\n{}",
                e.citation.path, e.citation.start_line, e.citation.end_line, e.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let reply = vox_core::agents::project_ideation::ideate(
        &state.project_ai_key,
        &state.project_ai_model,
        &p,
        input.message.trim(),
        &excerpts,
        &allowed,
    )
    .await
    .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let at = Utc::now().to_rfc3339();
    p.data.messages.push(ProjectMessage {
        id: Uuid::new_v4(),
        role: "user".into(),
        body: input.message,
        at: at.clone(),
        proposals: vec![],
        flow: None,
        diagram_id: None,
    });
    p.data.messages.push(ProjectMessage {
        id: Uuid::new_v4(),
        role: "assistant".into(),
        body: reply.reply,
        at,
        proposals: reply.proposals,
        flow: reply.flow,
        diagram_id: None,
    });
    if p.data.messages.len() > 100 {
        p.data.messages.drain(..p.data.messages.len() - 100);
    }
    if !valid(&p.name, &p.data) {
        return Err(StatusCode::BAD_GATEWAY);
    }
    let updated = repo
        .update(actor.user_id, id, p.revision, &p.name, &p.data)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::CONFLICT)?;
    notify(&state, actor.user_id, id);
    Ok(Json(updated))
}
#[utoipa::path(post, path="/v1/me/projects/{id}/plans/{plan}/attach", tag="projects", params(("id"=Uuid,Path),("plan"=Uuid,Path)), responses((status=204)))]
pub async fn attach(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path((id, plan)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode, StatusCode> {
    let _p = ProjectRepository(state.pool.clone())
        .get(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let rows=sqlx::query("UPDATE spaces SET project_id=$1,updated_at=now() WHERE id=$2 AND user_id=$3 AND state!='dropped' AND run_state!='running'").bind(id).bind(plan).bind(actor.user_id).execute(&state.pool).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?.rows_affected();
    if rows == 0 {
        return Err(StatusCode::CONFLICT);
    }
    notify(&state, actor.user_id, id);
    Ok(StatusCode::NO_CONTENT)
}

fn investigation_model(state: &ApiState) -> String {
    std::env::var("VOX_PROJECT_AGENT_MODEL")
        .ok()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 100
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
        .unwrap_or_else(|| state.project_ai_model.clone())
}
#[utoipa::path(get, path="/v1/me/projects/{id}/agent", tag="projects", params(("id"=Uuid,Path)), responses((status=200)))]
pub async fn agent_config(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    ProjectRepository(state.pool.clone())
        .get(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    Ok(Json(
        json!({"model":investigation_model(&state),"available":!state.project_ai_key.is_empty()}),
    ))
}
fn model_budget(user: Uuid) -> Result<(), StatusCode> {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    static BUDGET: OnceLock<Mutex<HashMap<Uuid, (Instant, u16)>>> = OnceLock::new();
    let mut budgets = BUDGET
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let now = Instant::now();
    budgets.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(600));
    if budgets.len() >= 10000 && !budgets.contains_key(&user) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let (_, count) = budgets.entry(user).or_insert((now, 0));
    if *count >= 180 {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    *count += 1;
    Ok(())
}
#[utoipa::path(post, path="/v1/me/projects/{id}/agent/{action}", tag="projects", params(("id"=Uuid,Path),("action"=String,Path)), responses((status=200)))]
pub async fn agent_model(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Path((id, action)): Path<(Uuid, String)>,
    Json(body): Json<serde_json::Value>,
) -> Result<axum::response::Response, StatusCode> {
    if !matches!(
        action.as_str(),
        "generateContent" | "streamGenerateContent" | "countTokens"
    ) {
        return Err(StatusCode::BAD_REQUEST);
    }
    if serde_json::to_vec(&body)
        .map_err(|_| StatusCode::BAD_REQUEST)?
        .len()
        > 2_000_000
    {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    ProjectRepository(state.pool.clone())
        .get(actor.user_id, id)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .ok_or(StatusCode::NOT_FOUND)?;
    model_budget(actor.user_id)?;
    if state.project_ai_key.is_empty() {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let model = investigation_model(&state);
    let mut url =
        format!("https://generativelanguage.googleapis.com/v1beta/models/{model}:{action}");
    if action == "streamGenerateContent" {
        url.push_str("?alt=sse");
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(180))
        .build()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let response = client
        .post(url)
        .header("x-goog-api-key", &state.project_ai_key)
        .json(&body)
        .send()
        .await
        .map_err(|_| StatusCode::BAD_GATEWAY)?;
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("application/json")
        .to_string();
    axum::response::Response::builder()
        .status(status.as_u16())
        .header("content-type", content_type)
        .body(axum::body::Body::from_stream(response.bytes_stream()))
        .map_err(|_| StatusCode::BAD_GATEWAY)
}
