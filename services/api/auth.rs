/**
* Authentication and authorization middleware for incoming API requests.
*/
use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use uuid::Uuid;
use vox_core::domain::identity::Actor;

pub async fn extract_actor(
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {

    let user_id = if let Some(hdr) = req.headers().get("X-Vox-User-Id") {
        hdr.to_str()
            .ok()
            .and_then(|s| Uuid::parse_str(s).ok())
            .unwrap_or_else(Uuid::nil)
    } else {
        Uuid::nil()
    };

    let actor = Actor::user(user_id);
    req.extensions_mut().insert(actor);
    Ok(next.run(req).await)
}
