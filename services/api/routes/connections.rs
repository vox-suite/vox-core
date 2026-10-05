use axum::{
    Extension, Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::Response,
};
use serde::Deserialize;
use uuid::Uuid;
use vox_core::{
    domain::identity::Actor,
    fresh_connections::{
        ConnectionItem, ConnectorDescriptor, FreshConnectionsService, PreferencesRequest,
        RefreshResponse, SetupStatusResponse, StartConnectionRequest, StartConnectionResponse,
    },
};

#[derive(Deserialize)]
pub struct GoogleCallbackQuery {
    pub code: Option<String>,
    pub error: Option<String>,
    pub state: String,
}

#[utoipa::path(post, path = "/v1/me/connectors/list", tag = "connections", security(("bearer_auth" = [])), responses((status = 200, body = Vec<ConnectorDescriptor>)))]
pub async fn list_connectors(
    State(svc): State<FreshConnectionsService>,
) -> Json<Vec<ConnectorDescriptor>> {
    Json(svc.list_connectors())
}

#[utoipa::path(post, path = "/v1/me/connections/list", tag = "connections", security(("bearer_auth" = [])), responses((status = 200, body = Vec<ConnectionItem>)))]
pub async fn list_connections(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
) -> Result<Json<Vec<ConnectionItem>>, (StatusCode, String)> {
    svc.list_connections(actor.user_id)
        .await
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Connection request failed".to_string(),
            )
        })
}

#[utoipa::path(post, path = "/v1/me/connections/start", tag = "connections", security(("bearer_auth" = [])), request_body = StartConnectionRequest, responses((status = 200, body = StartConnectionResponse)))]
pub async fn start_connection(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<StartConnectionRequest>,
) -> Result<Json<StartConnectionResponse>, (StatusCode, String)> {
    svc.start(actor.user_id, req).await.map(Json).map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "Connection request failed".to_string(),
        )
    })
}

#[utoipa::path(post, path = "/v1/me/connections/setup/{id}/status", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = SetupStatusResponse)))]
pub async fn setup_status(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<SetupStatusResponse>, (StatusCode, String)> {
    svc.get_setup_status(actor.user_id, id)
        .await
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                "Connection request failed".to_string(),
            )
        })
}

#[utoipa::path(post, path = "/v1/me/connections/{id}/preferences", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), request_body = PreferencesRequest, responses((status = 200, body = ConnectionItem)))]
pub async fn update_preferences(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
    Json(req): Json<PreferencesRequest>,
) -> Result<Json<ConnectionItem>, (StatusCode, String)> {
    svc.update_preferences(actor.user_id, id, req)
        .await
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Connection request failed".to_string(),
            )
        })
}

#[utoipa::path(post, path = "/v1/me/connections/{id}/refresh", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = RefreshResponse)))]
pub async fn refresh_connection(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<RefreshResponse>, (StatusCode, String)> {
    let user_id = actor.user_id;
    tokio::spawn(async move { svc.refresh(user_id, id).await })
        .await
        .unwrap_or_else(|_| {
            Err(vox_core::fresh_connections::FreshConnectionError::Provider(
                "refresh task aborted".into(),
            ))
        })
        .map(Json)
        .map_err(|error| {
            tracing::warn!(connection_id = %id, error = %error, "connection refresh failed");
            match error {
                vox_core::fresh_connections::FreshConnectionError::Invalid(message)
                    if message.contains("already syncing") =>
                {
                    (
                        StatusCode::CONFLICT,
                        "A sync is already running for this account".to_string(),
                    )
                }
                _ => (
                    StatusCode::BAD_GATEWAY,
                    "Connection request failed".to_string(),
                ),
            }
        })
}

#[utoipa::path(post, path = "/v1/me/connections/{id}/disconnect", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 204)))]
pub async fn disconnect_connection(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    svc.disconnect(actor.user_id, id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Connection request failed".to_string(),
            )
        })
}

pub async fn google_callback(
    State(svc): State<FreshConnectionsService>,
    Query(q): Query<GoogleCallbackQuery>,
) -> Response {
    let result = if q.error.is_some() || q.code.is_none() {
        let _ = svc.fail_google_setup(&q.state).await;
        Err(vox_core::fresh_connections::FreshConnectionError::Unauthorized)
    } else {
        svc.handle_google_callback(q.code.as_deref().unwrap_or_default(), &q.state)
            .await
    };
    super::callback_page::page("google", result.is_ok())
}

#[utoipa::path(post, path = "/v1/me/connections/setup/{id}/cancel", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 204)))]
pub async fn cancel_setup(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    svc.cancel_setup(actor.user_id, id)
        .await
        .map(|_| StatusCode::NO_CONTENT)
        .map_err(|_| (StatusCode::BAD_REQUEST, "Unable to cancel setup".into()))
}

pub async fn connector_callback(
    State(svc): State<FreshConnectionsService>,
    Path(connector): Path<String>,
    Query(q): Query<GoogleCallbackQuery>,
) -> Response {
    let code = if q.error.is_some() {
        None
    } else {
        q.code.as_deref()
    };
    let result = match connector.as_str() {
        "spotify" | "youtube" => {
            svc.handle_personal_callback(&connector, code, &q.state)
                .await
        }
        _ => svc.handle_food_callback(&connector, code, &q.state).await,
    };
    super::callback_page::page(&connector, result.is_ok())
}

#[derive(Deserialize, utoipa::ToSchema)]
pub struct YouTubeHistoryImportRequest {
    pub consent: bool,
    pub history: serde_json::Value,
}

#[utoipa::path(post, path = "/v1/me/connections/youtube/history/import", tag = "connections", security(("bearer_auth" = [])), request_body = YouTubeHistoryImportRequest, responses((status = 200, body = serde_json::Value)))]
pub async fn import_youtube_history(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Json(req): Json<YouTubeHistoryImportRequest>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    svc.import_youtube_history(actor.user_id, req.history, req.consent)
        .await
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Unable to import YouTube history".into(),
            )
        })
}

#[utoipa::path(post, path = "/v1/me/connections/{id}/read", tag = "connections", security(("bearer_auth" = [])), params(("id" = Uuid, Path)), responses((status = 200, body = serde_json::Value)))]
pub async fn read_personal(
    State(svc): State<FreshConnectionsService>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    svc.read_personal(actor.user_id, id)
        .await
        .map(Json)
        .map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "Unable to read connected account".into(),
            )
        })
}
