/**
* Core conversational engine handling user turns, greetings, and LLM responses.
*/
use super::{CompleteConversationRequest, ConversationId, RespondRequest, RespondResponse};
use crate::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder, PromptMessage},
    },
    db::Db,
    identity::{IdentityError, IdentityService, ResolvedUserContext, ResourceOwner, UserId},
    memory::MemoryService,
};
use futures_util::{FutureExt, Stream, StreamExt, stream};
use sqlx::Row;
use std::{pin::Pin, sync::Arc};
use uuid::Uuid;

type OpeningTask =
    futures_util::future::Shared<futures_util::future::BoxFuture<'static, Result<(), String>>>;

pub type ConversationTextStream =
    Pin<Box<dyn Stream<Item = Result<String, ConversationError>> + Send>>;

#[derive(Clone)]
pub struct ConversationService {
    pub(super) db: Db,
    agent: Arc<dyn ConversationResponder>,
    pub(super) identities: IdentityService,
    memory: MemoryService,
    pub(super) speculative: super::speculation::SpeculationCache,
    openings: Arc<tokio::sync::Mutex<std::collections::HashMap<String, OpeningTask>>>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("invalid conversation request")]
    Invalid,
    #[error("conversation not found")]
    NotFound,
    #[error("conversation storage unavailable: {0}")]
    Database(#[from] sqlx::Error),
    #[error("conversation agent unavailable")]
    Agent(#[from] AgentError),
    #[error("conversation identity conflict")]
    IdentityConflict,
    #[error("conversation identity unavailable")]
    Identity(#[from] IdentityError),
}

impl ConversationService {
    pub fn new(db: Db, agent: Arc<dyn ConversationResponder>) -> Self {
        let memory = MemoryService::new(db.clone(), None);
        Self::with_memory(db, agent, memory)
    }

    pub fn with_memory(
        db: Db,
        agent: Arc<dyn ConversationResponder>,
        memory: MemoryService,
    ) -> Self {
        Self {
            identities: IdentityService::new(db.clone()),
            db,
            agent,
            memory,
            openings: Arc::default(),
            speculative: Default::default(),
        }
    }

    pub async fn resolve_context_for_user(
        &self,
        user_id: Uuid,
    ) -> Result<ResolvedUserContext, IdentityError> {
        self.identities.resolve_for_user(user_id).await
    }

    pub async fn respond(
        &self,
        context: ResolvedUserContext,
        request: RespondRequest,
    ) -> Result<RespondResponse, ConversationError> {
        let selected_agent = self
            .selected_agent(&context, &request.agent_external_key)
            .await?;
        let owner = context.owner();
        self.wait_for_opening(owner, &request.identity, &request.external_conversation_id)
            .await?;
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            tracing::warn!(
                channel_empty = request.identity.channel.trim().is_empty(),
                identity_empty = request.identity.external_id.trim().is_empty(),
                conversation_id_empty = request.external_conversation_id.trim().is_empty(),
                text_empty = request.text.trim().is_empty(),
                "CONVERSATION_INVALID: empty request field"
            );
            return Err(ConversationError::Invalid);
        }
        if !self.register_final(owner, &request).await {
            tracing::warn!(turn_id = ?request.turn_id, revision = ?request.revision, "CONVERSATION_INVALID: stale revision");
            return Err(ConversationError::Invalid);
        }

        let (conversation_id, active_user_id) = self
            .resolve_conversation(
                owner,
                &request.identity.channel,
                &request.external_conversation_id,
                &request.agent_external_key,
            )
            .await?;

        let (prior_messages_res, known_name_res) = tokio::join!(
            self.load_recent_messages(conversation_id),
            self.memory.get_user_name(active_user_id),
        );
        let prior_messages = prior_messages_res?;
        let known_name = known_name_res?;

        let needs_onboarding = known_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty());
        let is_voice = crate::agents::conversation::is_voice_channel(&request.identity.channel);
        let is_inbound_connect = is_voice
            && (request.text.trim() == "The call just connected. Greet the user."
                || (prior_messages.is_empty()
                    && request.initiation_context.as_deref()
                        == Some("The call just connected. Greet the user.")));

        if is_inbound_connect {
            let known_name = self.memory.get_user_name(active_user_id).await?;
            let greeting = if let Some(ref name) = known_name {
                format!("Hello {}! How can I help you today?", name.trim())
            } else {
                "Hi there! It seems you're calling for the first time. How can I help you?"
                    .to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting)
                .await?;
            return Ok(RespondResponse {
                conversation_id,
                text: greeting,
                task: None,
            });
        }

        if !self.is_current(owner, &request).await {
            return Err(ConversationError::Invalid);
        }
        let active_owner = context.owner();
        let saved_request = request.clone();
        let projection_started = std::time::Instant::now();
        let user_context = self
            .memory
            .load(context.owner(), &selected_agent.definition.external_key)
            .await?;
        tracing::info!(
            projection_ms = projection_started.elapsed().as_millis() as u64,
            context_bytes = user_context.len(),
            "CORE_CONTEXT_PROJECTION"
        );
        let task_capture = crate::agents::tools::library::TaskCapture::default();
        let task_context = context.clone();
        let text = self
            .agent
            .respond(ConversationPrompt {
                correlation: crate::agents::tools::timing::TurnCorrelation {
                    conversation_id: Some(request.external_conversation_id.clone()),
                    turn_id: request.turn_id.clone(),
                    revision: request.revision,
                },
                task_capture: task_capture.clone(),
                context,
                selected_agent,
                user_id: active_user_id,
                user_name: known_name.clone(),
                owner: active_owner,
                channel: request.identity.channel.clone(),
                user_context,
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
                needs_onboarding,
                tts_provider: request.tts_provider.clone(),
                filler: request.filler.clone(),
                conversation_id: Some(conversation_id.0),
            })
            .await?;
        if !self.is_current(owner, &saved_request).await {
            return Err(ConversationError::Invalid);
        }
        self.append_message(conversation_id, "user", saved_request.text.trim())
            .await?;
        self.append_message(conversation_id, "assistant", text.trim())
            .await?;
        let task = if let Some(id) = task_capture.task_id() {
            crate::durable_tasks::DurableTaskService::new(self.db.clone())
                .get(&task_context, id)
                .await
                .ok()
        } else {
            None
        };
        Ok(RespondResponse {
            conversation_id,
            text,
            task,
        })
    }

    pub async fn respond_stream(
        &self,
        context: ResolvedUserContext,
        request: RespondRequest,
    ) -> Result<ConversationTextStream, ConversationError> {
        let phase_started = std::time::Instant::now();
        let selected_agent = self
            .selected_agent(&context, &request.agent_external_key)
            .await?;
        let agent_select_ms = phase_started.elapsed().as_millis() as u64;
        let owner = context.owner();
        let opening_started = std::time::Instant::now();
        self.wait_for_opening(owner, &request.identity, &request.external_conversation_id)
            .await?;
        let opening_wait_ms = opening_started.elapsed().as_millis() as u64;
        tracing::info!(
            agent_select_ms,
            opening_wait_ms,
            "CORE_AGENT_AND_OPENING_PREPARATION"
        );
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            tracing::warn!(
                channel_empty = request.identity.channel.trim().is_empty(),
                identity_empty = request.identity.external_id.trim().is_empty(),
                conversation_id_empty = request.external_conversation_id.trim().is_empty(),
                text_empty = request.text.trim().is_empty(),
                "CONVERSATION_INVALID: empty request field"
            );
            return Err(ConversationError::Invalid);
        }
        if crate::agents::conversation::is_voice_channel(&request.identity.channel)
            && request.text.trim() == "The call just connected. Greet the user."
        {
            return self.cached_opening(context, request).await;
        }
        let request_started = std::time::Instant::now();
        let identity_resolved = std::time::Instant::now();
        if !self.register_final(owner, &request).await {
            tracing::warn!(turn_id = ?request.turn_id, revision = ?request.revision, "CONVERSATION_INVALID: stale revision");
            return Err(ConversationError::Invalid);
        }

        let (conversation_id, active_user_id) = self
            .resolve_conversation(
                owner,
                &request.identity.channel,
                &request.external_conversation_id,
                &request.agent_external_key,
            )
            .await?;

        let conversation_resolved = std::time::Instant::now();
        let (prior_messages_res, known_name_res) = tokio::join!(
            self.load_recent_messages(conversation_id),
            self.memory.get_user_name(active_user_id),
        );
        let prior_messages = prior_messages_res?;
        let known_name = known_name_res?;
        let history_loaded = std::time::Instant::now();

        let needs_onboarding = known_name
            .as_deref()
            .is_none_or(|name| name.trim().is_empty());
        tracing::info!(
            agent_select_ms,
            opening_wait_ms,
            conversation_ms = conversation_resolved
                .duration_since(identity_resolved)
                .as_millis(),
            history_messages = prior_messages.len(),
            history_bytes = prior_messages
                .iter()
                .map(|message| message.text.len())
                .sum::<usize>(),
            history_and_name_ms = history_loaded
                .duration_since(conversation_resolved)
                .as_millis(),
            "CORE_TURN_PREPARATION"
        );
        let is_voice = crate::agents::conversation::is_voice_channel(&request.identity.channel);
        let is_inbound_connect = is_voice
            && (request.text.trim() == "The call just connected. Greet the user."
                || (prior_messages.is_empty()
                    && request.initiation_context.as_deref()
                        == Some("The call just connected. Greet the user.")));

        if is_inbound_connect {
            let known_name = self.memory.get_user_name(active_user_id).await?;
            let name_loaded = std::time::Instant::now();
            let greeting = if let Some(ref name) = known_name {
                format!("Hello {}! How can I help you today?", name.trim())
            } else {
                "Hi there! It seems you're calling for the first time. How can I help you?"
                    .to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting)
                .await?;

            tracing::info!(
                conversation_id = %conversation_id.0,
                external_conversation_id = %request.external_conversation_id,
                conversation_ms = conversation_resolved.duration_since(identity_resolved).as_millis(),
                history_and_name_ms = history_loaded.duration_since(conversation_resolved).as_millis(),
                name_ms = name_loaded.duration_since(history_loaded).as_millis(),
                assistant_message_ms = name_loaded.elapsed().as_millis(),
                total_ms = request_started.elapsed().as_millis(),
                "CORE_GREETING_METRICS"
            );

            let chunks = if let Some(ref name) = known_name {
                vec![
                    Ok(format!("Hello {}! ", name.trim())),
                    Ok("How can I help you today?".to_string()),
                ]
            } else {
                vec![
                    Ok("Hi there! ".to_string()),
                    Ok(
                        "It seems you're calling for the first time. How can I help you?"
                            .to_string(),
                    ),
                ]
            };
            return Ok(Box::pin(futures_util::stream::iter(chunks)));
        }

        if !self.is_current(owner, &request).await {
            return Err(ConversationError::Invalid);
        }
        let active_owner = context.owner();
        let projection_started = std::time::Instant::now();
        let user_context = self
            .memory
            .load(context.owner(), &selected_agent.definition.external_key)
            .await?;

        tracing::info!(
            projection_ms = projection_started.elapsed().as_millis() as u64,
            context_bytes = user_context.len(),
            "CORE_CONTEXT_PROJECTION"
        );
        let saved_request = request.clone();
        let agent = self.agent.clone();
        let model_stream = stream::once(async move {
            agent
                .respond_stream(ConversationPrompt {
                    correlation: crate::agents::tools::timing::TurnCorrelation {
                        conversation_id: Some(request.external_conversation_id.clone()),
                        turn_id: request.turn_id.clone(),
                        revision: request.revision,
                    },
                    task_capture: Default::default(),
                    context,
                    selected_agent,
                    user_id: active_user_id,
                    user_name: known_name.clone(),
                    owner: active_owner,
                    channel: request.identity.channel,
                    user_context,
                    recent_messages: prior_messages,
                    user_text: request.text,
                    initiation_context: request.initiation_context,
                    needs_onboarding,
                    tts_provider: request.tts_provider,
                    filler: request.filler,
                    conversation_id: Some(conversation_id.0),
                })
                .await
                .map_err(ConversationError::from)
        })
        .map(|result| match result {
            Ok(stream) => Box::pin(stream.map(|item| item.map_err(ConversationError::from)))
                as ConversationTextStream,
            Err(error) => Box::pin(stream::once(async move { Err(error) })),
        })
        .flatten();
        let stream: ConversationTextStream = Box::pin(model_stream);

        let service = self.clone();
        let out_stream = stream::unfold(
            (
                stream,
                String::new(),
                false,
                service,
                conversation_id,
                active_user_id,
                owner,
                saved_request,
            ),
            |(mut stream, mut full_text, finished, service, conv_id, uid, owner, request)| async move {
                if finished || !service.is_current(owner, &request).await {
                    return None;
                }
                let next = stream.next().await;
                if !service.is_current(owner, &request).await {
                    return None;
                }
                match next {
                    Some(Ok(chunk)) => {
                        full_text.push_str(&chunk);
                        Some((
                            Ok(chunk),
                            (
                                stream, full_text, false, service, conv_id, uid, owner, request,
                            ),
                        ))
                    }
                    Some(Err(err)) => Some((
                        Err(err),
                        (
                            stream, full_text, true, service, conv_id, uid, owner, request,
                        ),
                    )),
                    None => {
                        let text_to_save = full_text.trim().to_string();
                        if !text_to_save.is_empty() && service.is_current(owner, &request).await {
                            let _ = service
                                .persist_current_turn(owner, &request, conv_id, &text_to_save)
                                .await;
                        }
                        None
                    }
                }
            },
        );

        Ok(Box::pin(out_stream))
    }

    fn opening_key(
        owner: ResourceOwner,
        identity: &crate::identity::ChannelIdentity,
        external_id: &str,
    ) -> String {
        serde_json::to_string(&(
            owner,
            identity.channel.trim(),
            identity.external_id.trim(),
            external_id.trim(),
        ))
        .unwrap()
    }

    async fn wait_for_opening(
        &self,
        owner: ResourceOwner,
        identity: &crate::identity::ChannelIdentity,
        external_id: &str,
    ) -> Result<(), ConversationError> {
        let pending = self
            .openings
            .lock()
            .await
            .get(&Self::opening_key(owner, identity, external_id))
            .cloned();
        if let Some(pending) = pending
            && let Err(error) = pending.await
        {
            tracing::warn!(
                %error,
                channel = %identity.channel,
                external_conversation_id = %external_id,
                "Opening initialization failed; continuing without persisted greeting"
            );
        }
        Ok(())
    }

    async fn cached_opening(
        &self,
        context: ResolvedUserContext,
        request: RespondRequest,
    ) -> Result<ConversationTextStream, ConversationError> {
        let started = std::time::Instant::now();
        let owner = context.owner();
        // A channel identity is presentation data, never a cache authority key.
        let name = self.memory.get_user_name(owner.user_id).await?;
        let name_ms = started.elapsed().as_millis() as u64;
        let conversation_started = std::time::Instant::now();
        self.resolve_conversation(
            owner,
            &request.identity.channel,
            &request.external_conversation_id,
            &request.agent_external_key,
        )
        .await?;
        let conversation_ms = conversation_started.elapsed().as_millis() as u64;
        let name = name
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let greeting = match name {
            Some(name) => format!("Hello {name}! How can I help you today?"),
            None => "Hi there! It seems you're calling for the first time. How can I help you?"
                .to_owned(),
        };
        let channel = request.identity.channel.clone();
        let key = Self::opening_key(owner, &request.identity, &request.external_conversation_id);
        let mut openings = self.openings.lock().await;
        if !openings.contains_key(&key) {
            let service = self.clone();
            let text = greeting.clone();
            let pending = async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(30),
                    service.persist_opening(owner, request, text),
                )
                .await
                .map_err(|_| "opening initialization timed out".to_owned())?
                .map_err(|error| error.to_string())
            }
            .boxed()
            .shared();
            openings.insert(key.clone(), pending.clone());
            let service = self.clone();
            tokio::spawn(async move {
                if let Err(error) = pending.await {
                    tracing::error!(%error, "Background greeting initialization failed");
                }
                service.openings.lock().await.remove(&key);
            });
        }
        drop(openings);
        tracing::info!(
            user_id = %owner.user_id.0,
            channel = %channel,
            name_known = name.is_some(), name_ms, conversation_ms,
            total_ms = started.elapsed().as_millis(),
            "CORE_CACHED_GREETING_METRICS"
        );
        Ok(Box::pin(stream::once(async move { Ok(greeting) })))
    }

    async fn persist_opening(
        &self,
        owner: ResourceOwner,
        request: RespondRequest,
        greeting: String,
    ) -> Result<(), ConversationError> {
        let started = std::time::Instant::now();
        let (conversation_id, _) = self
            .resolve_conversation(
                owner,
                &request.identity.channel,
                &request.external_conversation_id,
                &request.agent_external_key,
            )
            .await?;
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(conversation_id.0.to_string())
            .execute(&mut *tx)
            .await?;
        let exists = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM messages WHERE conversation_id = $1)",
        )
        .bind(conversation_id.0)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            sqlx::query("INSERT INTO messages (conversation_id, sequence_number, role, text) VALUES ($1, 1, 'user', $2), ($1, 2, 'assistant', $3)")
                .bind(conversation_id.0).bind(request.text.trim()).bind(greeting).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        tracing::info!(external_conversation_id = %request.external_conversation_id, total_ms = started.elapsed().as_millis(), "CORE_OPENING_INITIALIZED");
        Ok(())
    }

    pub async fn complete(
        &self,
        context: ResolvedUserContext,
        request: CompleteConversationRequest,
    ) -> Result<(), ConversationError> {
        self.selected_agent(&context, &request.agent_external_key)
            .await?;
        let owner = context.owner();
        self.wait_for_opening(owner, &request.identity, &request.external_conversation_id)
            .await?;
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let conversation = sqlx::query(
            "SELECT id FROM conversations \
             WHERE channel = $1 AND external_conversation_id = $2 AND user_context_id = $3 AND agent_external_key = $4",
        )
        .bind(request.identity.channel.trim())
        .bind(request.external_conversation_id.trim())
        .bind(owner.user_context_id.0)
        .bind(&request.agent_external_key)
        .fetch_optional(self.db.pool())
        .await?;

        let conversation_id = match conversation {
            Some(row) => row.get::<Uuid, _>("id"),
            None => return Ok(()),
        };

        let mut tx = self.db.pool().begin().await?;
        let changed = sqlx::query(
            "UPDATE conversations \
             SET state = 'completed', \
                 completed_at = COALESCE(completed_at, now()), \
                 updated_at = now() \
             WHERE id = $1 AND state = 'active'",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?
        .rows_affected();

        if changed == 1 {
            sqlx::query(
                "INSERT INTO jobs (kind, payload_reference_id, user_id, user_context_id) \
                 SELECT 'summarize_conversation', $1, $2, $3 \
                 WHERE NOT EXISTS ( \
                     SELECT 1 FROM jobs WHERE kind = 'summarize_conversation' AND payload_reference_id = $1 \
                 )",
            )
            .bind(conversation_id)
            .bind(owner.user_id.0)
            .bind(owner.user_context_id.0)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn load_recent_messages(
        &self,
        conversation_id: ConversationId,
    ) -> Result<Vec<PromptMessage>, ConversationError> {
        let rows = sqlx::query(
            "SELECT role, text FROM messages WHERE conversation_id = $1 ORDER BY sequence_number ASC",
        )
        .bind(conversation_id.0)
        .fetch_all(self.db.pool())
        .await?;

        Ok(rows
            .into_iter()
            .map(|row| PromptMessage {
                role: row.get("role"),
                text: row.get("text"),
            })
            .collect())
    }

    pub(super) async fn selected_agent(
        &self,
        context: &ResolvedUserContext,
        key: &str,
    ) -> Result<crate::agent_registry::SelectedAgent, ConversationError> {
        crate::agent_registry::AgentRegistry::new(self.db.clone())
            .selected_for_context(context, key)
            .await
            .map_err(|error| {
                tracing::warn!(
                    %error,
                    agent_external_key = key,
                    deployment_id = %context.subject.deployment_id.0,
                    "CONVERSATION_INVALID: agent not selected for deployment"
                );
                ConversationError::Invalid
            })
    }

    pub(super) async fn resolve_conversation(
        &self,
        owner: ResourceOwner,
        channel: &str,
        external_id: &str,
        agent_key: &str,
    ) -> Result<(ConversationId, UserId), ConversationError> {
        let row = sqlx::query(
            "INSERT INTO conversations (user_context_id,user_id,channel,external_conversation_id,agent_external_key)
             VALUES ($1,$2,$3,$4,$5)
             ON CONFLICT (user_context_id,channel,external_conversation_id)
             DO UPDATE SET updated_at=now()
             WHERE conversations.agent_external_key=EXCLUDED.agent_external_key
               AND conversations.user_id=EXCLUDED.user_id AND conversations.state='active'
             RETURNING id,user_id",
        ).bind(owner.user_context_id.0).bind(owner.user_id.0)
         .bind(channel.trim()).bind(external_id.trim()).bind(agent_key)
         .fetch_optional(self.db.pool()).await?.ok_or(ConversationError::IdentityConflict)?;
        Ok((ConversationId(row.get("id")), UserId(row.get("user_id"))))
    }

    pub(super) async fn append_turn(
        &self,
        conversation_id: ConversationId,
        user_text: &str,
        assistant_text: &str,
    ) -> Result<(), ConversationError> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(conversation_id.0.to_string())
            .execute(&mut *tx)
            .await?;
        let sequence = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM messages WHERE conversation_id = $1",
        )
        .bind(conversation_id.0)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("INSERT INTO messages (conversation_id,sequence_number,role,text) VALUES ($1,$2,'user',$3),($1,$2+1,'assistant',$4)")
            .bind(conversation_id.0).bind(sequence).bind(user_text).bind(assistant_text).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }

    async fn append_message(
        &self,
        conversation_id: ConversationId,
        role: &str,
        text: &str,
    ) -> Result<(), ConversationError> {
        let mut tx = self.db.pool().begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(conversation_id.0.to_string())
            .execute(&mut *tx)
            .await?;
        let sequence = sqlx::query_scalar::<_, i64>(
            "SELECT COALESCE(MAX(sequence_number), 0) + 1 FROM messages WHERE conversation_id = $1",
        )
        .bind(conversation_id.0)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO messages (conversation_id, sequence_number, role, text) VALUES ($1, $2, $3, $4)",
        )
        .bind(conversation_id.0)
        .bind(sequence)
        .bind(role)
        .bind(text)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }
}
