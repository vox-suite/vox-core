use axum::{Extension, Json, extract::State};
use vox_core::{domain::identity::Actor, map_scene::MapScene, realtime::UserEventHub};

pub async fn get_map_scene(
    State(hub): State<UserEventHub>,
    Extension(actor): Extension<Actor>,
) -> Json<MapScene> {
    Json(hub.scene(actor.user_id))
}
