use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};
use vox_core::{domain::identity::Actor, identity::UserId, memory::MemoryService};

pub async fn refresh_minimal_user_after(
    State(memory): State<MemoryService>,
    req: Request,
    next: Next,
) -> Response {
    let actor = req.extensions().get::<Actor>().cloned();
    let response = next.run(req).await;
    if response.status().is_success()
        && let Some(actor) = actor
    {
        let _ = memory.refresh_minimal_user(UserId(actor.user_id)).await;
    }
    response
}
