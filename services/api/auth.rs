/**
* Authentication and authorization middleware for incoming API requests.
*/
use axum::{
    extract::{Request, State},
    http::{
        StatusCode,
        header::{AUTHORIZATION, SEC_WEBSOCKET_PROTOCOL},
    },
    middleware::Next,
    response::Response,
};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;
use vox_core::domain::identity::Actor;

const LIVE_SOCKET_PATH: &str = "/v1/me/events/socket";
const SUBPROTOCOL_TOKEN_PREFIX: &str = "bearer.";
const WEB_SCOPE_PREFIXES: [&str; 9] = [
    "/v1/spans",
    "/v1/integrations/authorize",
    "/v1/collections",
    "/v1/me/schemas",
    "/v1/me/charts",
    "/v1/me/spaces",
    "/v1/me/connectors",
    "/v1/me/connections",
    LIVE_SOCKET_PATH,
];

fn web_scope_allows(path: &str) -> bool {
    WEB_SCOPE_PREFIXES.iter().any(|prefix| {
        path.strip_prefix(prefix)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

fn bearer_token(req: &Request, path: &str) -> Option<String> {
    let headers = req.headers();
    if let Some(value) = headers.get(AUTHORIZATION).and_then(|v| v.to_str().ok()) {
        return value
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string);
    }
    if path != LIVE_SOCKET_PATH {
        return None;
    }
    headers
        .get(SEC_WEBSOCKET_PROTOCOL)
        .and_then(|v| v.to_str().ok())?
        .split(',')
        .map(str::trim)
        .find_map(|item| item.strip_prefix(SUBPROTOCOL_TOKEN_PREFIX))
        .filter(|t| !t.is_empty())
        .map(str::to_string)
}

pub async fn extract_actor(
    State(pool): State<PgPool>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let path = req.uri().path().to_string();
    let token = bearer_token(&req, &path).ok_or(StatusCode::UNAUTHORIZED)?;
    let token = token.as_str();

    if token.split('.').count() == 3 {
        let claims = crate::identity_token::verify_id_token(token).await?;
        let user_id = Uuid::parse_str(&claims.subject).map_err(|_| StatusCode::UNAUTHORIZED)?;
        let display_name = claims
            .name
            .as_deref()
            .or(claims.email.as_deref())
            .unwrap_or("Vox User");
        let known = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM user_contexts WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if !known {
            let mut tx = pool
                .begin()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            sqlx::query(
                "INSERT INTO users (id, status, display_name) \
                 VALUES ($1, 'active', $2) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .bind(user_id)
            .bind(display_name)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            sqlx::query(
                "INSERT INTO user_contexts (deployment_id, host_app_id, host_user_id, user_id) \
                 SELECT d.id, h.id, $1::text, $2::uuid \
                 FROM platform_deployments d \
                 JOIN host_apps h ON h.deployment_id = d.id \
                 WHERE d.external_key = 'vox.standalone.deployment' \
                   AND h.external_key = 'vox.standalone.web' \
                 ON CONFLICT (user_id) DO NOTHING",
            )
            .bind(user_id.to_string())
            .bind(user_id)
            .execute(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            let has_context = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM user_contexts WHERE user_id = $1)",
            )
            .bind(user_id)
            .fetch_one(&mut *tx)
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
            if !has_context {
                return Err(StatusCode::INTERNAL_SERVER_ERROR);
            }
            tx.commit()
                .await
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        }

        let actor = Actor::user(user_id);
        req.extensions_mut().insert(actor);
        return Ok(next.run(req).await);
    }

    // 2. Otherwise check session tokens in auth_sessions
    let token_hash = hex::encode(Sha256::digest(token.as_bytes()));

    let session_row = sqlx::query(
        "SELECT user_id, device_id, scope FROM auth_sessions \
         WHERE token_hash = $1 \
           AND expires_at > now() \
           AND revoked_at IS NULL",
    )
    .bind(token_hash)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
    .ok_or(StatusCode::UNAUTHORIZED)?;

    let user_id: Uuid = session_row.get("user_id");
    let device_id: Option<Uuid> = session_row.get("device_id");
    let scope: String = session_row.get("scope");
    if scope == "web" && !web_scope_allows(&path) {
        return Err(StatusCode::FORBIDDEN);
    }

    let actor = if let Some(dev_id) = device_id {
        Actor::device(user_id, dev_id)
    } else {
        Actor::user(user_id)
    };
    req.extensions_mut().insert(actor);

    Ok(next.run(req).await)
}

#[cfg(test)]
mod integration_boundary_tests {
    use super::*;
    #[tokio::test]
    #[ignore = "requires dedicated INTEGRATION_TEST_DATABASE_URL with Core migrations"]
    async fn delegated_bearer_cannot_authenticate_general_user_route() {
        let pool = PgPool::connect(&std::env::var("INTEGRATION_TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        let user = sqlx::query_scalar::<_, Uuid>("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
        let grant=sqlx::query_scalar::<_,Uuid>("INSERT INTO integration_grants(user_id,client_id,collection_ids,board_ids,allow_create_plans) VALUES($1,'share_to_action','{}','{}',true) RETURNING id").bind(user).fetch_one(&pool).await.unwrap();
        let token = format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple());
        sqlx::query("INSERT INTO integration_tokens(access_hash,refresh_hash,grant_id,access_expires_at,refresh_expires_at) VALUES($1,$2,$3,now()+interval '1 hour',now()+interval '1 day')").bind(vox_core::integrations::hash(&token)).bind(Uuid::new_v4().to_string()).bind(grant).execute(&pool).await.unwrap();
        let app = axum::Router::new()
            .route(
                "/v1/spans",
                axum::routing::post(|| async { StatusCode::OK }),
            )
            .layer(axum::middleware::from_fn_with_state(
                pool.clone(),
                extract_actor,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = reqwest::Client::new()
            .post(format!("http://{address}/v1/spans"))
            .bearer_auth(&token)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        server.abort();
        sqlx::query("DELETE FROM users WHERE id=$1")
            .bind(user)
            .execute(&pool)
            .await
            .unwrap();
    }
}
