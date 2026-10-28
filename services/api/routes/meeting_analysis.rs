use crate::state::ApiState;
use axum::{Extension, Json, extract::State, http::StatusCode};
use vox_core::domain::identity::Actor;
use vox_shared::meetings::{AnalysisRequest, AnalysisResponse, validate_request};
fn budget(user: uuid::Uuid) -> Result<(), StatusCode> {
    use std::{
        collections::HashMap,
        sync::{Mutex, OnceLock},
        time::{Duration, Instant},
    };
    static BUDGET: OnceLock<Mutex<HashMap<uuid::Uuid, (Instant, u8)>>> = OnceLock::new();
    let mut state = BUDGET
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let now = Instant::now();
    state.retain(|_, (at, _)| now.duration_since(*at) < Duration::from_secs(600));
    if state.len() >= 10000 && !state.contains_key(&user) {
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    }
    let (_, count) = state.entry(user).or_insert((now, 0));
    if *count >= 5 {
        return Err(StatusCode::TOO_MANY_REQUESTS);
    }
    *count += 1;
    Ok(())
}
#[utoipa::path(post,path="/v1/meetings/analyze",tag="meetings",request_body=AnalysisRequest,responses((status=200,body=AnalysisResponse),(status=400,description="Invalid/oversized text or missing evidence"),(status=429,description="Summary request budget exceeded"),(status=503,description="Text model unavailable")))]
pub async fn analyze(
    State(state): State<ApiState>,
    Extension(actor): Extension<Actor>,
    Json(input): Json<AnalysisRequest>,
) -> Result<Json<AnalysisResponse>, (StatusCode, Json<serde_json::Value>)> {
    let error = |status, message: String| (status, Json(serde_json::json!({"error":message})));
    validate_request(&input).map_err(|e| error(StatusCode::BAD_REQUEST, e))?;
    if input.timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "Choose a valid meeting timezone".into(),
        ));
    }
    if state.project_ai_key.is_empty() {
        return Err(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Cloud text summaries are not configured".into(),
        ));
    }
    budget(actor.user_id)
        .map_err(|s| error(s, "Please wait before requesting another summary".into()))?;
    let result = vox_core::usage::scope(
        actor.user_id,
        tokio::time::timeout(
            std::time::Duration::from_secs(60),
            vox_core::agents::meeting_analysis::generate(
                &state.project_ai_key,
                &state.project_ai_model,
                &input,
            ),
        ),
    )
    .await
    .map_err(|_| {
        error(
            StatusCode::GATEWAY_TIMEOUT,
            "Summary timed out; your transcript is unchanged".into(),
        )
    })?
    .map_err(|e| error(StatusCode::BAD_GATEWAY, e))?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    #[test]
    fn summary_budget_is_bounded_and_scoped_to_each_account() {
        let owner = uuid::Uuid::new_v4();
        for _ in 0..5 {
            assert!(super::budget(owner).is_ok());
        }
        assert_eq!(
            super::budget(owner),
            Err(axum::http::StatusCode::TOO_MANY_REQUESTS)
        );
        assert!(super::budget(uuid::Uuid::new_v4()).is_ok());
    }
}
