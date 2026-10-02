/**
* HTTP endpoints for conversation turns, audio streams, and history.
*/
use super::{AppState, remote_extensions::context};
use crate::conversations::{
    CompleteConversationRequest, RespondRequest, RespondResponse, service::ConversationError,
};
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct AuthenticatedRespondRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub conversation: RespondRequest,
}

#[derive(Deserialize)]
pub struct AuthenticatedCompleteRequest {
    pub host_context: crate::host_trust::HostContextRequest,
    #[serde(flatten)]
    pub conversation: CompleteConversationRequest,
}

pub async fn respond(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.respond(context, request.conversation).await {
        Ok(response) => (StatusCode::OK, Json::<RespondResponse>(response)).into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConversationError::Agent(_)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(
            ConversationError::Database(_)
            | ConversationError::Identity(_)
            | ConversationError::IdentityConflict
            | ConversationError::NotFound,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn respond_stream(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedRespondRequest>,
) -> Response {
    let started = std::time::Instant::now();
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let context_ms = started.elapsed().as_millis() as u64;
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let turn_id = request.conversation.turn_id.clone();
    let conversation_id = request.conversation.external_conversation_id.clone();
    let channel = request.conversation.identity.channel.clone();
    let prompt_chars = request.conversation.text.len();
    let service_started = std::time::Instant::now();
    let task_capture = crate::agents::tools::library::TaskCapture::default();
    let task_context = context.clone();
    match service
        .respond_stream_captured(context, request.conversation, task_capture.clone())
        .await
    {
        Ok(stream) => {
            let service_ms = service_started.elapsed().as_millis() as u64;
            let preparation_ms = started.elapsed().as_millis() as u64;
            let mut first_text_ms: Option<u64> = None;
            let deltas = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let chars = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
            let (deltas_done, chars_done) = (deltas.clone(), chars.clone());
            let first_turn_id = turn_id.clone();
            let first_conversation_id = conversation_id.clone();
            let sse_stream = stream.map(move |item| {
                if let Ok(delta) = &item
                    && !delta.is_empty()
                {
                    deltas.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    chars.fetch_add(delta.len() as u64, std::sync::atomic::Ordering::Relaxed);
                    if first_text_ms.is_none() {
                        let first = started.elapsed().as_millis() as u64;
                        first_text_ms = Some(first);
                        tracing::info!(
                            turn_id = ?first_turn_id,
                            conversation_id = %first_conversation_id,
                            context_ms,
                            service_prepare_ms = service_ms,
                            preparation_ms,
                            first_text_ms = first,
                            "CORE_STREAM_FIRST_TEXT"
                        );
                    }
                }
                stream_event(item)
            });
            let done_stream = futures_util::stream::once(async move {
                tracing::info!(
                    turn_id = ?turn_id,
                    conversation_id = %conversation_id,
                    channel = %channel,
                    prompt_chars,
                    reply_deltas = deltas_done.load(std::sync::atomic::Ordering::Relaxed),
                    reply_chars = chars_done.load(std::sync::atomic::Ordering::Relaxed),
                    context_ms,
                    service_prepare_ms = service_ms,
                    stream_total_ms = started.elapsed().as_millis() as u64,
                    "CORE_STREAM_DONE"
                );
                Ok::<_, std::convert::Infallible>("data: [DONE]\n\n".to_string())
            });
            let tasks = task_events(service.clone(), task_context, task_capture);
            let full_stream = sse_stream.chain(tasks).chain(done_stream);

            axum::response::Response::builder()
                .status(StatusCode::OK)
                .header("content-type", "text/event-stream")
                .header("cache-control", "no-cache")
                .header("connection", "keep-alive")
                .header("x-vox-prepare-ms", preparation_ms.to_string())
                .header("x-vox-context-ms", context_ms.to_string())
                .body(axum::body::Body::from_stream(full_stream))
                .unwrap()
        }
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(ConversationError::Agent(_)) => StatusCode::BAD_GATEWAY.into_response(),
        Err(
            ConversationError::Database(_)
            | ConversationError::Identity(_)
            | ConversationError::IdentityConflict
            | ConversationError::NotFound,
        ) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

pub async fn complete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<AuthenticatedCompleteRequest>,
) -> Response {
    let Some(context) = context(state.host_trust.as_deref(), &headers, request.host_context).await
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let Some(service) = state.conversations.as_ref() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    match service.complete(context, request.conversation).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(ConversationError::Invalid) => StatusCode::BAD_REQUEST.into_response(),
        Err(_) => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

fn task_events(
    service: std::sync::Arc<crate::conversations::service::ConversationService>,
    context: crate::identity::ResolvedUserContext,
    capture: crate::agents::tools::library::TaskCapture,
) -> impl futures_util::Stream<Item = Result<String, std::convert::Infallible>> + Send {
    futures_util::stream::once(async move {
        service.captured_task(&context, &capture).await.map(|task| {
            format!(
                "event: task\ndata: {}\n\n",
                serde_json::json!({"task":task})
            )
        })
    })
    .filter_map(|event| async move { event.map(Ok) })
}

fn stream_event(
    item: Result<String, ConversationError>,
) -> Result<String, std::convert::Infallible> {
    Ok(match item {
        Ok(delta) => format!("data: {}\n\n", serde_json::json!({"delta":delta})),
        Err(_) => "event: error\ndata: {\"error\":\"agent error\"}\n\n".into(),
    })
}

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::{
        agent_registry::{AgentMutation, AgentRegistry},
        agents::{
            AgentError,
            conversation::{AgentStream, ConversationPrompt, ConversationResponder},
            tools::library::{AgentLibrary, LibraryRequest, TaskCapture},
        },
        db::Db,
        delegation::{CapabilityReference, CapabilitySelection},
        durable_tasks::DurableTaskService,
        identity::IdentityService,
    };
    use async_trait::async_trait;
    use std::sync::Arc;
    struct Model {
        db: Db,
        specialist: String,
        connection: uuid::Uuid,
    }
    #[async_trait]
    impl ConversationResponder for Model {
        async fn respond(&self, _: ConversationPrompt) -> Result<String, AgentError> {
            Err(AgentError::Provider)
        }
        async fn respond_stream(&self, p: ConversationPrompt) -> Result<AgentStream, AgentError> {
            let library = AgentLibrary::new(
                Some(self.db.clone()),
                None,
                p.context,
                p.selected_agent.definition.external_key,
            )
            .with_task_capture(p.task_capture);
            library
                .invoke(LibraryRequest::Delegate {
                    specialist_agent_key: self.specialist.clone(),
                    brief: "Read selected repository facts".into(),
                    scope: CapabilitySelection {
                        capabilities: vec![CapabilityReference {
                            connection_id: self.connection,
                            capability_external_key: "repository.read".into(),
                        }],
                    },
                    permission_id: None,
                })
                .await
                .map_err(|_| AgentError::Provider)?;
            let mut items = vec![Ok("Please review the specialist scope.".into())];
            if p.user_text.contains("fail") {
                items.push(Err(AgentError::Provider));
            }
            Ok(Box::pin(futures_util::stream::iter(items)))
        }
    }
    #[tokio::test]
    #[ignore = "requires isolated PostgreSQL TEST_DATABASE_URL"]
    async fn streaming_task_events_follow_text_and_errors_with_exact_owned_handles() {
        let db = Db::connect(&std::env::var("TEST_DATABASE_URL").unwrap())
            .await
            .unwrap();
        db.migrate().await.unwrap();
        let user: uuid::Uuid = sqlx::query_scalar("INSERT INTO users DEFAULT VALUES RETURNING id")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let context = IdentityService::new(db.clone())
            .resolve_for_user(user)
            .await
            .unwrap();
        let registry = AgentRegistry::new(db.clone());
        let personal = registry.owned_for_context(&context).await.unwrap()[0].clone();
        registry
            .mutate_owned(
                &context,
                AgentMutation::Create {
                    name: "Streaming specialist".into(),
                    instructions: "Read selected facts".into(),
                },
            )
            .await
            .unwrap();
        let specialist = registry
            .owned_for_context(&context)
            .await
            .unwrap()
            .into_iter()
            .find(|a| !a.definition.is_default)
            .unwrap();
        let extension:uuid::Uuid=sqlx::query_scalar("INSERT INTO remote_extensions(user_context_id,external_key,display_name,protocol,endpoint_url,operator_id,operator_name,conformance_status,operator_enabled,lifecycle_state) VALUES($1,'stream-fixture','Stream fixture','mcp','https://example.com/mcp','fixture','Fixture','passed',true,'active') RETURNING id").bind(context.id.0).fetch_one(db.pool()).await.unwrap();
        let capability = serde_json::json!({"external_key":"repository.read","display_name":"Repository read","effect":"read","consequential":false,"input_schema":{"type":"object"},"data_recipients":[],"access_needs":[],"supported_regions":[],"optional_guarantees":{}});
        sqlx::query("INSERT INTO remote_extension_versions(extension_id,version,endpoint_url,operator_id,operator_name,capabilities,conformance_status) VALUES($1,1,'https://example.com/mcp','fixture','Fixture',$2,'passed')").bind(extension).bind(serde_json::json!([capability])).execute(db.pool()).await.unwrap();
        let connection:uuid::Uuid=sqlx::query_scalar("INSERT INTO external_connections(user_context_id,remote_extension_id,external_account_hash,credential_custody,authorization_state,authorized_capabilities) VALUES($1,$2,$3,'platform_held','authorized',ARRAY['repository.read']) RETURNING id").bind(context.id.0).bind(extension).bind(vec![6u8;32]).fetch_one(db.pool()).await.unwrap();
        sqlx::query("INSERT INTO agent_capability_grants(user_context_id,agent_definition_id,connection_id,capability_external_key) VALUES($1,$2,$3,'repository.read')").bind(context.id.0).bind(specialist.definition.id).bind(connection).execute(db.pool()).await.unwrap();
        let service = Arc::new(crate::conversations::service::ConversationService::new(
            db.clone(),
            Arc::new(Model {
                db: db.clone(),
                specialist: specialist.definition.external_key,
                connection,
            }),
        ));
        for text in [
            "Have the specialist produce scoped repository facts",
            "Have the specialist fail after creating governed work",
        ] {
            let capture = TaskCapture::default();
            let request = crate::conversations::RespondRequest {
                agent_external_key: personal.definition.external_key.clone(),
                identity: crate::identity::ChannelIdentity {
                    channel: "web".into(),
                    external_id: context.subject.host_user_id.clone(),
                },
                external_conversation_id: text.into(),
                text: text.into(),
                initiation_context: None,
                turn_id: None,
                revision: None,
                tts_provider: None,
                filler: None,
            };
            let stream = service
                .respond_stream_captured(context.clone(), request, capture.clone())
                .await
                .unwrap();
            let events: Vec<_> = stream
                .map(stream_event)
                .chain(task_events(
                    service.clone(),
                    context.clone(),
                    capture.clone(),
                ))
                .chain(futures_util::stream::once(async {
                    Ok("data: [DONE]\n\n".into())
                }))
                .collect()
                .await;
            let frames: Vec<String> = events.into_iter().map(Result::unwrap).collect();
            assert!(frames[0].starts_with("data: {\"delta\":"));
            assert_eq!(frames.last().unwrap(), "data: [DONE]\n\n");
            let task_frame = &frames[frames.len() - 2];
            assert!(
                task_frame.starts_with("event: task\ndata: "),
                "frames={frames:?}, capture={:?}",
                capture.task_id()
            );
            let task: serde_json::Value = serde_json::from_str(
                task_frame
                    .strip_prefix("event: task\ndata: ")
                    .unwrap()
                    .trim(),
            )
            .unwrap();
            assert_eq!(
                task["task"]["id"],
                serde_json::json!(capture.task_id().unwrap())
            );
            assert_eq!(
                task["task"]["result"]["checkpoint"]["code"],
                "delegation_consent_required"
            );
            if text.contains("fail") {
                assert_eq!(
                    frames[1],
                    "event: error\ndata: {\"error\":\"agent error\"}\n\n"
                );
            }
            let mut foreign = context.clone();
            foreign.id = crate::identity::UserContextId(uuid::Uuid::new_v4());
            assert!(
                task_events(service.clone(), foreign, capture.clone())
                    .collect::<Vec<_>>()
                    .await
                    .is_empty()
            );
            let id = capture.task_id().unwrap();
            DurableTaskService::new(db.clone())
                .cancel(&context, id)
                .await
                .unwrap();
            let cancelled = task_events(service.clone(), context.clone(), capture)
                .collect::<Vec<_>>()
                .await;
            assert!(
                cancelled[0]
                    .as_ref()
                    .unwrap()
                    .contains("\"state\":\"cancelled\"")
            );
        }
        let capture = TaskCapture::default();
        let request = crate::conversations::RespondRequest {
            agent_external_key: personal.definition.external_key,
            identity: crate::identity::ChannelIdentity {
                channel: "web".into(),
                external_id: context.subject.host_user_id.clone(),
            },
            external_conversation_id: "disconnect proof".into(),
            text: "Have the specialist preserve durable work after disconnect".into(),
            initiation_context: None,
            turn_id: None,
            revision: None,
            tts_provider: None,
            filler: None,
        };
        let mut disconnected = service
            .respond_stream_captured(context.clone(), request, capture.clone())
            .await
            .unwrap();
        assert!(disconnected.next().await.unwrap().is_ok());
        drop(disconnected);
        let task = DurableTaskService::new(db)
            .get(&context, capture.task_id().unwrap())
            .await
            .unwrap();
        assert_eq!(
            task.state,
            crate::durable_tasks::RunState::Waiting,
            "dropping text stream does not cancel governed durable work"
        );
        assert!(
            task_events(service, context, TaskCapture::default())
                .collect::<Vec<_>>()
                .await
                .is_empty()
        );
    }
}
