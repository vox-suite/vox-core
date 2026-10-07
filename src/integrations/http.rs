use super::*;
use axum::{
    Extension, Json, Router,
    extract::{Query, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        let code = match self {
            Self::Invalid => StatusCode::BAD_REQUEST,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Database(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        (code, Json(serde_json::json!({"error":self.to_string()}))).into_response()
    }
}
pub fn public_router(s: IntegrationService) -> Router {
    Router::new()
        .route("/integrations/authorize", get(page))
        .route("/v1/integrations/token", post(token))
        .with_state(s.clone())
        .merge(
            Router::new()
                .route("/v1/integrations/context", get(context))
                .route("/v1/integrations/plans", post(plan))
                .route("/v1/integrations/revoke", post(revoke))
                .layer(middleware::from_fn_with_state(s.clone(), delegated))
                .with_state(s),
        )
}
pub fn authorization_router(s: IntegrationService) -> Router {
    Router::new()
        .route("/v1/integrations/authorize", post(authorize))
        .with_state(s)
}
async fn delegated(
    State(s): State<IntegrationService>,
    mut r: Request,
    next: Next,
) -> Result<Response, Error> {
    let token = r
        .headers()
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(Error::Forbidden)?;
    let grant = s.authenticate(token).await?;
    r.extensions_mut().insert(grant);
    Ok(next.run(r).await)
}
async fn authorize(
    State(s): State<IntegrationService>,
    Extension(actor): Extension<Actor>,
    Json(a): Json<Authorization>,
) -> Result<Json<serde_json::Value>, Error> {
    Ok(Json(
        serde_json::json!({"redirect_url":s.authorize(&actor,a).await?}),
    ))
}
async fn token(
    State(s): State<IntegrationService>,
    Json(r): Json<TokenRequest>,
) -> Result<Json<TokenResponse>, Error> {
    Ok(Json(s.token(r).await?))
}
async fn context(
    State(s): State<IntegrationService>,
    Extension(g): Extension<Grant>,
) -> Result<Json<serde_json::Value>, Error> {
    Ok(Json(s.context(&g).await?))
}
async fn plan(
    State(s): State<IntegrationService>,
    Extension(g): Extension<Grant>,
    Json(p): Json<Plan>,
) -> Result<Json<serde_json::Value>, Error> {
    Ok(Json(serde_json::json!({"id":s.plan(&g,p).await?})))
}
async fn revoke(
    State(s): State<IntegrationService>,
    Extension(g): Extension<Grant>,
) -> Result<Json<serde_json::Value>, Error> {
    s.revoke(&g).await?;
    Ok(Json(serde_json::json!({"revoked":true})))
}
#[derive(Deserialize)]
struct PageQuery {
    client_id: String,
    redirect_uri: String,
    state: String,
    code_challenge: String,
    code_challenge_method: Option<String>,
}
async fn page(
    State(s): State<IntegrationService>,
    Query(q): Query<PageQuery>,
) -> Result<Response, Error> {
    s.validate_client(&q.client_id, &q.redirect_uri)?;
    if q.code_challenge_method.as_deref() != Some("S256")
        || q.state.is_empty()
        || q.code_challenge.len() != 43
    {
        return Err(Error::Invalid);
    }
    let google = std::env::var("GOOGLE_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty())
        .ok_or(Error::Unavailable)?;
    let config=serde_json::json!({"client_id":q.client_id,"redirect_uri":q.redirect_uri,"state":q.state,"code_challenge":q.code_challenge,"google_client_id":google}).to_string().replace('<',"\\u003c");
    Ok((
        [
            ("x-frame-options", "DENY"),
            ("content-security-policy", "frame-ancestors 'none'"),
            ("referrer-policy", "no-referrer"),
            ("cache-control", "no-store"),
        ],
        Html(include_str!("consent.html").replace("__CONFIG__", &config)),
    )
        .into_response())
}
