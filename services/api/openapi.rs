/**
* OpenAPI documentation and contract definitions for API endpoints.
*/
use axum::{Json, response::IntoResponse};
use serde_json::Value;

pub async fn get_openapi_spec() -> impl IntoResponse {
    let spec = include_str!("../../contracts/openapi.json");
    let json: Value = serde_json::from_str(spec).unwrap_or_else(|_| {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": { "title": "Vox Core API", "version": "1.0.0" }
        })
    });
    Json(json)
}
