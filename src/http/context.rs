use super::host_apps::assertion_from_headers;
use crate::host_trust::{HostContextRequest, HostTrustService};
use axum::http::HeaderMap;
use chrono::Utc;
pub(super) async fn context(
    trust: Option<&HostTrustService>,
    headers: &HeaderMap,
    request: HostContextRequest,
) -> Option<crate::identity::ResolvedUserContext> {
    let trust = trust?;
    let assertion = assertion_from_headers(headers).ok()?;
    let origin = headers.get("origin").and_then(|v| v.to_str().ok());
    trust
        .resolve_authenticated_context(&assertion, &request, origin, Utc::now())
        .await
        .ok()
}
