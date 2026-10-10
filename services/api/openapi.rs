use axum::{Json, response::IntoResponse};
use serde_json::{Value, json};
use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(paths(
    crate::routes::connections::list_connectors,
    crate::routes::connections::read_personal,
    crate::routes::connections::import_youtube_history,
    crate::routes::connections::import_maps_timeline,
    crate::routes::connections::list_connections,
    crate::routes::connections::start_connection,
    crate::routes::connections::setup_status,
    crate::routes::connections::update_preferences,
    crate::routes::connections::refresh_connection,
    crate::routes::connections::disconnect_connection,
    crate::routes::connections::cancel_setup,
    crate::routes::auth::mint_web_token,
    crate::routes::spans::list_spans,
    crate::routes::spans::list_span_days,
    crate::routes::spans::list_span_day,
    crate::routes::spans::create_span,
    crate::routes::spans::get_span,
    crate::routes::spans::update_span,
    crate::routes::spans::delete_span,
    crate::routes::collections::list_collections,
    crate::routes::collections::create_collection,
    crate::routes::collections::get_collection,
    crate::routes::collections::update_collection,
    crate::routes::collections::archive_collection,
    crate::routes::collections::add_collection_span,
    crate::routes::collections::remove_collection_span,
    crate::routes::schemas::create_schema_version,
    crate::routes::schemas::get_schema_by_name,
    crate::routes::schemas::list_schemas,
    crate::routes::pulse::get_canvas,
    crate::routes::pulse::list_measurements,
    crate::routes::pulse::discover,
    crate::routes::pulse::preview,
    crate::routes::pulse::save,
    crate::routes::pulse::list_charts,
    crate::routes::pulse::get_chart,
    crate::routes::pulse::update_chart,
    crate::routes::pulse::delete_chart,
    crate::routes::pulse::dismiss,
    crate::routes::pulse::list_dismissals,
    crate::routes::pulse::undismiss,
    crate::routes::spaces::create_space,
    crate::routes::spaces::list_spaces,
    crate::routes::spaces::get_space,
    crate::routes::spaces::drop_space,
    crate::routes::spaces::send_space_chat,
    crate::routes::spaces::stop_space,
    crate::routes::spaces::retry_space_node,
    crate::routes::spaces::commit_space,
    crate::routes::spaces::list_space_messages,
    crate::routes::spaces::update_space_node,
    crate::routes::timeline::day_counts,
    crate::routes::timeline::list_groups,
    crate::routes::timeline::list_event_types,
    crate::routes::timeline::create_event_type,
    crate::routes::timeline::query_events,
    crate::routes::timeline::ingest_event,
    crate::routes::updates::list_updates,
    crate::routes::updates::get_update,
    crate::routes::updates::mark_update_read,
    crate::routes::updates::dismiss_update,
    crate::routes::updates::resolve_update,
    crate::routes::updates::retry_job,
    crate::routes::updates::provide_job_input,
    crate::routes::gmail::device_historical_import,
    crate::routes::gmail::device_access,
    crate::routes::takeout::upload_takeout
))]
struct ApiDoc;

pub fn build_spec() -> Value {
    let mut spec: Value = serde_json::from_str(include_str!("../../contracts/openapi.base.json"))
        .unwrap_or_else(|_| {
            json!({
                "openapi": "3.1.0",
                "info": { "title": "Vox Core API", "version": "1.0.0" },
                "paths": {},
                "components": { "schemas": {} }
            })
        });
    let generated = serde_json::to_value(ApiDoc::openapi()).unwrap_or(Value::Null);
    merge_object(&mut spec, &generated, "paths");
    if let Some(paths) = spec.get_mut("paths").and_then(Value::as_object_mut) {
        paths.retain(|path,_| !path.starts_with("/v1/records"));
    }
    if let Some(schemas) = generated
        .pointer("/components/schemas")
        .and_then(Value::as_object)
    {
        let target = spec
            .pointer_mut("/components")
            .and_then(Value::as_object_mut)
            .map(|c| c.entry("schemas").or_insert_with(|| json!({})));
        if let Some(Value::Object(map)) = target {
            for (name, schema) in schemas {
                map.insert(name.clone(), schema.clone());
            }
        }
    }
    spec
}

fn merge_object(spec: &mut Value, generated: &Value, key: &str) {
    let Some(source) = generated.get(key).and_then(Value::as_object) else {
        return;
    };
    let target = spec
        .as_object_mut()
        .map(|root| root.entry(key).or_insert_with(|| json!({})));
    if let Some(Value::Object(map)) = target {
        for (name, value) in source {
            map.insert(name.clone(), value.clone());
        }
    }
}

pub async fn get_openapi_spec() -> impl IntoResponse {
    Json(build_spec())
}
