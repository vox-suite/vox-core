use axum::{Extension,Json,body::Bytes,extract::State,http::StatusCode,response::IntoResponse};
use chrono::{DateTime,Utc};
use serde::Serialize;
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use sqlx::PgPool;
use std::io::Read;
use uuid::Uuid;
use vox_core::{domain::{identity::Actor,timeline::{IngestTimelineEventInput,NewEvidenceItem}},storage::timeline::TimelineRepository};
const MAX_TAKEOUT_BYTES: usize = 100 * 1024 * 1024;
#[derive(Serialize,utoipa::ToSchema)]
pub struct TakeoutUploadResponse { pub youtube_records_imported: usize,pub maps_records_imported: usize,pub total_events_created: usize,pub skipped_records: usize,pub notes: Vec<String> }

#[utoipa::path(post,path="/v1/connectors/google/takeout/upload",request_body(content=Vec<u8>,content_type="application/octet-stream"),responses((status=200,body=TakeoutUploadResponse),(status=413,description="Export exceeds 100 MiB")))]
pub async fn upload_takeout(State(pool):State<PgPool>,Extension(actor):Extension<Actor>,body:Bytes)->Result<impl IntoResponse,StatusCode> {
    if body.is_empty() { return Err(StatusCode::BAD_REQUEST); }
    if body.len()>MAX_TAKEOUT_BYTES { return Err(StatusCode::PAYLOAD_TOO_LARGE); }
    static PARSERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> = std::sync::OnceLock::new();
    let parser = PARSERS.get_or_init(||std::sync::Arc::new(tokio::sync::Semaphore::new(1))).clone().try_acquire_owned().map_err(|_|StatusCode::TOO_MANY_REQUESTS)?;
    let files = tokio::task::spawn_blocking(move || {
        let _parser = parser;
        if body.starts_with(b"PK") { extract_zip_entries(&body) }
        else { serde_json::from_slice(&body).map(|v|vec![("direct.json".into(),v)]).map_err(|e|e.to_string()) }
    }).await.map_err(|_|StatusCode::INTERNAL_SERVER_ERROR)?.map_err(|_|StatusCode::BAD_REQUEST)?;
    let repo=TimelineRepository::new(pool.clone());
    let mut response=TakeoutUploadResponse { youtube_records_imported:0,maps_records_imported:0,total_events_created:0,skipped_records:0,notes:vec![] };
    if files.is_empty() { response.notes.push("No JSON data was found. Choose JSON format when creating the YouTube or Timeline export.".into()); }
    for (name,value) in files {
        let name_lower=name.to_ascii_lowercase();
        let mut candidates=Vec::new();
        if name_lower.contains("watch-history") || (name_lower=="direct.json" && value.as_array().is_some_and(|items|items.iter().any(|item|item.get("titleUrl").is_some()))) {
            if let Some(items)=value.as_array() {
                for row in items {
                    if let Some(event)=youtube_event(row) { candidates.push(("youtube_history",event));response.youtube_records_imported+=1; }
                    else { response.skipped_records+=1; }
                }
            }
        }
        if let Some(items)=value.get("timelineObjects").and_then(Value::as_array) {
            for row in items {
                if let Some(event)=legacy_maps_event(row) { candidates.push(("maps_timeline",event));response.maps_records_imported+=1; }
                else { response.skipped_records+=1; }
            }
        }
        let segments=value.get("semanticSegments").and_then(Value::as_array)
            .or_else(||value.as_array().filter(|a|a.iter().any(|v|v.get("visit").is_some() || v.get("activity").is_some())));
        if let Some(items)=segments {
            for row in items {
                if let Some(event)=modern_maps_event(row) { candidates.push(("maps_timeline",event));response.maps_records_imported+=1; }
                else { response.skipped_records+=1; }
            }
        }
        if candidates.is_empty() && (name_lower.contains("timeline") || name_lower.contains("settings") || name_lower.contains("backup")) {
            response.notes.push(format!("{name}: no readable visits or travel records; settings, edits and encrypted backups cannot establish location history."));
        }
        for chunk in candidates.chunks(100) {
            let mut tx=pool.begin().await.map_err(database_error)?;
            for (connector,input) in chunk {
                let key=input.dedupe_key.as_deref().ok_or(StatusCode::BAD_REQUEST)?;
                let metadata=json!({"filename":name,"source":"google_takeout","coverage":"partial_snapshot"});
                let record:Option<Uuid>=sqlx::query_scalar("INSERT INTO source_records(user_id,connector_id,source_record_id,record_hash,metadata) VALUES($1,$2,$3,$4,$5) ON CONFLICT(user_id,connector_id,source_record_id) DO NOTHING RETURNING id")
                    .bind(actor.user_id).bind(connector).bind(key).bind(format!("{:x}",Sha256::digest(serde_json::to_vec(&input.content).map_err(|_|StatusCode::BAD_REQUEST)?))).bind(&metadata).fetch_optional(&mut *tx).await.map_err(database_error)?;
                if let Some(record_id)=record {
                    let mut input=input.clone();
                    input.evidence=vec![NewEvidenceItem { source_record_id:Some(record_id),source_attachment_id:None,source_type:"google_takeout".into(),source_id:Some(key.into()),raw_reference:Some(name.clone()),observation_metadata:metadata }];
                    repo.ingest_event_in_transaction(&mut tx,actor.user_id,input).await.map_err(|e|{tracing::error!(%e,"Takeout event ingestion failed");StatusCode::BAD_REQUEST})?;
                    response.total_events_created+=1;
                }
            }
            tx.commit().await.map_err(database_error)?;
        }
    }
    for (connector,count) in [("youtube_history",response.youtube_records_imported),("maps_timeline",response.maps_records_imported)] {
        if count==0 {continue;}
        let connection:Uuid=sqlx::query_scalar("INSERT INTO vox_connections(user_id,connector_id,authorization_state,sync_timeline,last_synced_at,metadata) VALUES($1,$2,'authorized',true,now(),$3) ON CONFLICT(user_id,connector_id) DO UPDATE SET last_synced_at=now(),updated_at=now(),metadata=vox_connections.metadata || EXCLUDED.metadata RETURNING id")
            .bind(actor.user_id).bind(connector).bind(json!({"source":"manual_takeout","records_seen":count})).fetch_one(&pool).await.map_err(database_error)?;
        sqlx::query("INSERT INTO connector_coverage(user_id,connector_id,connection_id,sync_mode,is_healthy,last_checked_at,metadata) VALUES($1,$2,$3,'manual_takeout_upload',true,now(),$4) ON CONFLICT(user_id,connector_id) DO UPDATE SET last_checked_at=now(),metadata=EXCLUDED.metadata,updated_at=now()")
            .bind(actor.user_id).bind(connector).bind(connection).bind(json!({"coverage":"partial_snapshot","records_seen":count,"gaps":"unknown"})).execute(&pool).await.map_err(database_error)?;
    }
    if response.total_events_created==0 {response.notes.push("No new entries were created. The export may contain only unsupported records or entries already imported.".into());}
    sqlx::query("SELECT pg_notify('vox_timeline_updated',$1)").bind(json!({"type":"timeline_updated","user_id":actor.user_id}).to_string()).execute(&pool).await.map_err(database_error)?;
    Ok(Json(response))
}
fn database_error(error:sqlx::Error)->StatusCode {tracing::error!(%error,"Takeout database write failed");StatusCode::INTERNAL_SERVER_ERROR}
fn event(kind:&str,title:String,start:DateTime<Utc>,end:Option<DateTime<Utc>>,mut content:Value)->IngestTimelineEventInput {
    if let Some(end) = end { if let Some(object)=content.as_object_mut() { object.insert("duration_seconds".into(),json!((end-start).num_milliseconds() as f64 / 1000.0)); } }
    let identity=json!([kind,start.to_rfc3339(),end.map(|v|v.to_rfc3339()),content.get("video_id"),content.get("place_id")]);
    let key=format!("takeout:{:x}",Sha256::digest(identity.to_string().as_bytes()));
    IngestTimelineEventInput { event_type_id:None,event_type_value:Some(kind.into()),group_id:None,group_value:None,title,summary:None,occurred_at:start,ended_at:end,time_precision:"second".into(),source_timezone:None,content,confidence:1.0,dedupe_key:Some(key),evidence:vec![] }
}
fn time(v:&Value)->Option<DateTime<Utc>> {DateTime::parse_from_rfc3339(v.as_str()?).ok().map(|v|v.with_timezone(&Utc))}
fn youtube_event(row:&Value)->Option<IngestTimelineEventInput> {
    let title=row.get("title")?.as_str()?;
    if !title.starts_with("Watched ") {return None;}
    let url=reqwest::Url::parse(row.get("titleUrl")?.as_str()?).ok()?;
    if url.scheme()!="https" || !matches!(url.host_str(),Some("www.youtube.com"|"youtube.com"|"m.youtube.com")) {return None;}
    let video=url.query_pairs().find(|(k,_)|k=="v").map(|(_,v)|v.into_owned())?;
    let start=time(row.get("time")?)?;
    Some(event("video_watch",title.trim_start_matches("Watched ").into(),start,None,json!({"video_id":video,"url":url.as_str(),"channel":row.pointer("/subtitles/0/name"),"timing":"provider_timestamp","duration_known":false})))
}
fn legacy_maps_event(row:&Value)->Option<IngestTimelineEventInput> {
    if let Some(visit)=row.get("placeVisit") {
        let start=time(visit.pointer("/duration/startTimestamp")?)?;
        let end=time(visit.pointer("/duration/endTimestamp")?)?;
        if end<start {return None;}
        let location=visit.get("location")?;
        let title=location.get("name").or_else(||location.get("address"))?.as_str()?.to_string();
        return Some(event("visit",title,start,Some(end),json!({"place_id":location.get("placeId"),"address":location.get("address"),"latitude_e7":location.get("latitudeE7"),"longitude_e7":location.get("longitudeE7"),"timing":"known_interval"})));
    }
    let activity=row.get("activitySegment")?;
    let start=time(activity.pointer("/duration/startTimestamp")?)?;
    let end=time(activity.pointer("/duration/endTimestamp")?)?;
    if end<start {return None;}
    Some(event("travel",activity.get("activityType").and_then(Value::as_str).unwrap_or("Travel").into(),start,Some(end),json!({"activity_type":activity.get("activityType"),"distance_meters":activity.get("distance"),"start_location":activity.get("startLocation"),"end_location":activity.get("endLocation"),"timing":"known_interval"})))
}
fn modern_maps_event(row:&Value)->Option<IngestTimelineEventInput> {
    let start=time(row.get("startTime")?)?;
    let end=time(row.get("endTime")?)?;
    if end<start {return None;}
    if let Some(visit)=row.get("visit") {
        let candidate=visit.get("topCandidate")?;
        let place=candidate.get("placeID").or_else(||candidate.get("placeId"))?.as_str()?;
        let title=candidate.get("name").and_then(Value::as_str).unwrap_or("Recorded place visit");
        return Some(event("visit",title.into(),start,Some(end),json!({"place_id":place,"place_location":candidate.get("placeLocation"),"semantic_type":candidate.get("semanticType"),"timing":"known_interval"})));
    }
    let activity=row.get("activity")?;
    Some(event("travel","Recorded travel".into(),start,Some(end),json!({"activity_type":activity.pointer("/topCandidate/type"),"distance_meters":activity.get("distanceMeters"),"start_location":activity.get("start"),"end_location":activity.get("end"),"timing":"known_interval"})))
}
fn extract_zip_entries(bytes:&[u8])->Result<Vec<(String,Value)>,String> {
    let mut archive=zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e|e.to_string())?;
    if archive.len()>2048 {return Err("archive has too many entries".into());}
    let mut total=0u64;
    let mut entries=vec![];
    for i in 0..archive.len() {
        let mut file=archive.by_index(i).map_err(|e|e.to_string())?;
        if file.enclosed_name().is_none() || file.is_symlink() || file.encrypted() {return Err("unsafe or encrypted archive entry".into());}
        total=total.checked_add(file.size()).ok_or("archive size overflow")?;
        if total>500*1024*1024 || file.size()>100*1024*1024 || (file.compressed_size()>0 && file.size()/file.compressed_size()>200) {return Err("archive expansion exceeds limits".into());}
        if file.is_dir() || !file.name().to_ascii_lowercase().ends_with(".json") {continue;}
        let name=file.name().to_string();
        let mut data=vec![];
        file.by_ref().take(100*1024*1024+1).read_to_end(&mut data).map_err(|e|e.to_string())?;
        if data.len()>100*1024*1024 {return Err("entry exceeds limit".into());}
        entries.push((name,serde_json::from_slice(&data).map_err(|e|e.to_string())?));
    }
    Ok(entries)
}

pub async fn limit_import(request:axum::extract::Request,next:axum::middleware::Next) -> axum::response::Response {
    static IMPORTS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    let Ok(_permit) = IMPORTS.get_or_init(||tokio::sync::Semaphore::new(1)).try_acquire() else {
        return (StatusCode::TOO_MANY_REQUESTS,"An import is already running. Try again after it finishes.").into_response();
    };
    next.run(request).await
}
