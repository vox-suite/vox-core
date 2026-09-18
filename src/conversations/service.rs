use super::{CompleteConversationRequest, ConversationId, RespondRequest, RespondResponse};
use crate::{
    agents::{
        AgentError,
        conversation::{ConversationPrompt, ConversationResponder, PromptMessage},
    },
    db::Db,
    identity::{IdentityService, UserId},
    memory::MemoryService,
    voiceprint::{
        VoiceSignature, verify_phone_match, verify_voice_match_with_jev,
    },
};
use futures_util::{Stream, StreamExt, stream};
use serde::{Deserialize, Serialize};
use sqlx::Row;
use std::{pin::Pin, sync::Arc};
use uuid::Uuid;

pub type ConversationTextStream =
    Pin<Box<dyn Stream<Item = Result<String, ConversationError>> + Send>>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub enum VerificationState {
    AwaitingName {
        original_user_id: UserId,
        original_user_name: String,
        voice_signature: Option<VoiceSignature>,
    },
    AwaitingPhoneConfirm {
        original_user_id: UserId,
        candidate_user_id: UserId,
        candidate_name: String,
        voice_signature: Option<VoiceSignature>,
    },
}

pub enum VoiceVerificationOutcome {
    Intercept(String),
    Continue {
        active_user_id: UserId,
        needs_onboarding: bool,
    },
}

#[derive(Clone)]
pub struct ConversationService {
    db: Db,
    agent: Arc<dyn ConversationResponder>,
    identities: IdentityService,
    memory: MemoryService,
    jev: Option<crate::jev::JevClient>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConversationError {
    #[error("invalid conversation request")]
    Invalid,
    #[error("conversation not found")]
    NotFound,
    #[error("conversation storage unavailable")]
    Database(#[from] sqlx::Error),
    #[error("conversation agent unavailable")]
    Agent(#[from] AgentError),
    #[error("conversation identity conflict")]
    IdentityConflict,
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
            jev: None,
        }
    }

    pub fn with_jev(mut self, jev: crate::jev::JevClient) -> Self {
        self.jev = Some(jev);
        self
    }

    pub async fn respond(
        &self,
        request: RespondRequest,
    ) -> Result<RespondResponse, ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let user_id = self.identities.resolve(&request.identity).await?;

        // Seed name if provided via initiation_context (e.g. from WhatsApp profile)
        if let Some(init_ctx) = &request.initiation_context {
            if let Some(wa_name) = init_ctx.strip_prefix("whatsapp_name:") {
                let wa_name_clean = wa_name.trim();
                if !wa_name_clean.is_empty() {
                    let _ = self.memory.set_user_name(user_id, wa_name_clean).await;
                }
            }
        }

        let (conversation_id, mut active_user_id) = self
            .resolve_conversation(
                user_id,
                &request.identity.channel,
                &request.external_conversation_id,
            )
            .await?;

        // Load prior messages in this call to maintain a running session
        let prior_messages = self.load_recent_messages(conversation_id).await?;

        self.append_message(conversation_id, "user", request.text.trim())
            .await?;

        let outcome = self
            .process_voice_verification(active_user_id, conversation_id, &request, &prior_messages)
            .await?;

        let needs_onboarding = match outcome {
            VoiceVerificationOutcome::Intercept(reply) => {
                self.append_message(conversation_id, "assistant", &reply).await?;
                return Ok(RespondResponse {
                    conversation_id,
                    text: reply,
                });
            }
            VoiceVerificationOutcome::Continue {
                active_user_id: resolved_uid,
                needs_onboarding,
            } => {
                active_user_id = resolved_uid;
                needs_onboarding
            }
        };

        // Inbound call opening fast-path: Greet immediately (<1ms) using cached name from Redis
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
                "Hello! I'm Vox, your personal AI assistant. What should I call you?".to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting).await?;
            let _ = self.memory.refresh(active_user_id).await;
            return Ok(RespondResponse {
                conversation_id,
                text: greeting,
            });
        }

        let text = self
            .agent
            .respond(ConversationPrompt {
                user_id: active_user_id,
                channel: request.identity.channel.clone(),
                user_context: self.memory.load(active_user_id).await?,
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
                needs_onboarding,
            })
            .await?;
        self.append_message(conversation_id, "assistant", text.trim())
            .await?;
        let _ = self.memory.refresh(active_user_id).await;
        Ok(RespondResponse {
            conversation_id,
            text,
        })
    }

    pub async fn respond_stream(
        &self,
        request: RespondRequest,
    ) -> Result<ConversationTextStream, ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
            || request.text.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let user_id = self.identities.resolve(&request.identity).await?;

        if let Some(init_ctx) = &request.initiation_context {
            if let Some(wa_name) = init_ctx.strip_prefix("whatsapp_name:") {
                let wa_name_clean = wa_name.trim();
                if !wa_name_clean.is_empty() {
                    let _ = self.memory.set_user_name(user_id, wa_name_clean).await;
                }
            }
        }

        let (conversation_id, mut active_user_id) = self
            .resolve_conversation(
                user_id,
                &request.identity.channel,
                &request.external_conversation_id,
            )
            .await?;

        let prior_messages = self.load_recent_messages(conversation_id).await?;

        self.append_message(conversation_id, "user", request.text.trim())
            .await?;

        let outcome = self
            .process_voice_verification(active_user_id, conversation_id, &request, &prior_messages)
            .await?;

        let needs_onboarding = match outcome {
            VoiceVerificationOutcome::Intercept(reply) => {
                self.append_message(conversation_id, "assistant", &reply).await?;
                let chunks = vec![Ok(reply)];
                return Ok(Box::pin(futures_util::stream::iter(chunks)));
            }
            VoiceVerificationOutcome::Continue {
                active_user_id: resolved_uid,
                needs_onboarding,
            } => {
                active_user_id = resolved_uid;
                needs_onboarding
            }
        };

        // Inbound call opening fast-path: Stream greeting immediately (<1ms) using cached name from Redis
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
                "Hello! I'm Vox, your personal AI assistant. What should I call you?".to_string()
            };

            self.append_message(conversation_id, "assistant", &greeting).await?;
            let _ = self.memory.refresh(active_user_id).await;

            let chunks = if let Some(ref name) = known_name {
                vec![
                    Ok(format!("Hello {}! ", name.trim())),
                    Ok("How can I help you today?".to_string()),
                ]
            } else {
                vec![
                    Ok("Hello! I'm Vox, your personal AI assistant. ".to_string()),
                    Ok("What should I call you?".to_string()),
                ]
            };
            return Ok(Box::pin(futures_util::stream::iter(chunks)));
        }

        let user_context = self.memory.load(active_user_id).await?;

        let stream = self
            .agent
            .respond_stream(ConversationPrompt {
                user_id: active_user_id,
                channel: request.identity.channel.clone(),
                user_context,
                recent_messages: prior_messages,
                user_text: request.text,
                initiation_context: request.initiation_context,
                needs_onboarding,
            })
            .await?;

        let service = self.clone();
        let out_stream = stream::unfold(
            (stream, String::new(), false, service, conversation_id, active_user_id),
            |(mut stream, mut full_text, finished, service, conv_id, uid)| async move {
                if finished {
                    return None;
                }
                match stream.next().await {
                    Some(Ok(chunk)) => {
                        full_text.push_str(&chunk);
                        Some((Ok(chunk), (stream, full_text, false, service, conv_id, uid)))
                    }
                    Some(Err(err)) => {
                        Some((
                            Err(ConversationError::Agent(err)),
                            (stream, full_text, true, service, conv_id, uid),
                        ))
                    }
                    None => {
                        let text_to_save = full_text.trim().to_string();
                        if !text_to_save.is_empty() {
                            let _ = service.append_message(conv_id, "assistant", &text_to_save).await;
                            let _ = service.memory.refresh(uid).await;
                        }
                        None
                    }
                }
            },
        );

        Ok(Box::pin(out_stream))
    }

    async fn process_voice_verification(
        &self,
        user_id: UserId,
        conversation_id: ConversationId,
        request: &RespondRequest,
        prior_messages: &[PromptMessage],
    ) -> Result<VoiceVerificationOutcome, ConversationError> {
        let is_voice = crate::agents::conversation::is_voice_channel(&request.identity.channel);
        if !is_voice {
            let known_name = self.memory.get_user_name(user_id).await?;
            let has_name = known_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();
            return Ok(VoiceVerificationOutcome::Continue {
                active_user_id: user_id,
                needs_onboarding: !has_name,
            });
        }

        let parsed_sig = request
            .voice_signature
            .as_deref()
            .and_then(VoiceSignature::from_raw);

        // 1. Check existing verification state (Redis + message fallback)
        let state_opt: Option<VerificationState> = if let Some(state_json) = self.memory.get_verification_state(conversation_id.0).await {
            serde_json::from_str(&state_json).ok()
        } else {
            // Fallback: detect from last assistant message in prior_messages
            if let Some(last_msg) = prior_messages.last().filter(|m| m.role == "assistant") {
                let text = last_msg.text.trim();
                if text.starts_with("Your voice is not matching with ") && text.ends_with("What is your name?") {
                    let name_part = text
                        .strip_prefix("Your voice is not matching with ")
                        .and_then(|s| s.strip_suffix(". What is your name?"))
                        .unwrap_or("")
                        .trim();
                    Some(VerificationState::AwaitingName {
                        original_user_id: user_id,
                        original_user_name: name_part.to_string(),
                        voice_signature: parsed_sig.clone(),
                    })
                } else if text.starts_with("I found a matching profile for ") && text.ends_with("Can you tell me your phone number to confirm?") {
                    let name_part = text
                        .strip_prefix("I found a matching profile for ")
                        .and_then(|s| s.strip_suffix(" in my system. Can you tell me your phone number to confirm?"))
                        .unwrap_or("")
                        .trim();
                    if let Ok(Some(cand_uid)) = self.memory.find_user_by_name(name_part).await {
                        Some(VerificationState::AwaitingPhoneConfirm {
                            original_user_id: user_id,
                            candidate_user_id: cand_uid,
                            candidate_name: name_part.to_string(),
                            voice_signature: parsed_sig.clone(),
                        })
                    } else {
                        None
                    }
                } else {
                    None
                }
            } else {
                None
            }
        };

        // 2. Handle active verification state transitions
        match state_opt {
            Some(VerificationState::AwaitingPhoneConfirm {
                original_user_id: _,
                candidate_user_id,
                candidate_name,
                voice_signature,
            }) => {
                let candidate_phones = self.memory.voiceprints().get_user_phones(candidate_user_id).await?;
                let matched = candidate_phones
                    .iter()
                    .any(|p| verify_phone_match(&request.text, p));

                let sig_to_save = parsed_sig.or(voice_signature);

                if matched {
                    // Verified existing user! Switch conversation user to candidate_user_id
                    self.memory.voiceprints().update_conversation_user(conversation_id.0, candidate_user_id).await?;
                    if let Some(ref sig) = sig_to_save {
                        let _ = self.memory.set_voice_signature(candidate_user_id, sig).await;
                    }
                    self.memory.clear_verification_state(conversation_id.0).await;
                    let _ = self.memory.refresh(candidate_user_id).await;
                    let reply = format!(
                        "Awesome, verified! Hello {}! I've connected to your profile. How can I help you today?",
                        candidate_name
                    );
                    return Ok(VoiceVerificationOutcome::Intercept(reply));
                } else {
                    // Number didn't match -> create new user profile for this person
                    let new_user = self.memory.voiceprints().create_user_with_name(&candidate_name).await?;
                    self.memory.voiceprints().update_conversation_user(conversation_id.0, new_user).await?;
                    if let Some(ref sig) = sig_to_save {
                        let _ = self.memory.set_voice_signature(new_user, sig).await;
                    }
                    self.memory.clear_verification_state(conversation_id.0).await;
                    let _ = self.memory.refresh(new_user).await;
                    let reply = format!(
                        "No worries, that number didn't match, so I've created a new profile for you, {}. How can I help you today?",
                        candidate_name
                    );
                    return Ok(VoiceVerificationOutcome::Intercept(reply));
                }
            }

            Some(VerificationState::AwaitingName {
                original_user_id,
                original_user_name: _,
                voice_signature,
            }) => {
                let stated_name = extract_name_from_text(&request.text).unwrap_or_else(|| {
                    request.text.trim().trim_end_matches(['.', '!', '?']).to_string()
                });

                let sig_to_save = parsed_sig.or(voice_signature);

                // Look up in DB / Redis if candidate user already exists
                let existing_user = self.memory.find_user_by_name(&stated_name).await?;

                if let Some(cand_uid) = existing_user {
                    let next_state = VerificationState::AwaitingPhoneConfirm {
                        original_user_id,
                        candidate_user_id: cand_uid,
                        candidate_name: stated_name.clone(),
                        voice_signature: sig_to_save,
                    };
                    if let Ok(json) = serde_json::to_string(&next_state) {
                        self.memory.set_verification_state(conversation_id.0, &json).await;
                    }
                    let reply = format!(
                        "I found a matching profile for {} in my system. Can you tell me your phone number to confirm?",
                        stated_name
                    );
                    return Ok(VoiceVerificationOutcome::Intercept(reply));
                } else {
                    // Not found in DB/Redis -> create fresh user profile
                    let new_user = self.memory.voiceprints().create_user_with_name(&stated_name).await?;
                    self.memory.voiceprints().update_conversation_user(conversation_id.0, new_user).await?;
                    if let Some(ref sig) = sig_to_save {
                        let _ = self.memory.set_voice_signature(new_user, sig).await;
                    }
                    self.memory.clear_verification_state(conversation_id.0).await;
                    let _ = self.memory.refresh(new_user).await;
                    let reply = format!(
                        "Nice to meet you {}! I've created a new profile for you. How can I help you today?",
                        stated_name
                    );
                    return Ok(VoiceVerificationOutcome::Intercept(reply));
                }
            }

            None => {}
        }

        // 3. Normal turn processing / Voice biometric matching check
        let mut known_name = self.memory.get_user_name(user_id).await?;
        let mut has_name = known_name.as_deref().map(str::trim).filter(|s| !s.is_empty()).is_some();

        // Check if user is introducing themselves on first call
        if !has_name {
            if let Some(extracted_name) = extract_name_from_text(&request.text) {
                let _ = self.memory.set_user_name(user_id, &extracted_name).await;
                if let Some(ref sig) = parsed_sig {
                    let _ = self.memory.set_voice_signature(user_id, sig).await;
                }
                let reply = format!("Nice to meet you {}! How can I help you today?", extracted_name);
                return Ok(VoiceVerificationOutcome::Intercept(reply));
            }
        }

        // Check voice biometric match if name is known
        if let Some(ref name) = known_name {
            let stored_sig = self.memory.get_voice_signature(user_id).await?;
            if let Some(ref sig) = parsed_sig {
                if let Some(ref stored) = stored_sig {
                    let similarity = sig.cosine_similarity(stored);
                    let matches = verify_voice_match_with_jev(self.jev.as_ref(), similarity, name).await;
                    if !matches {
                        // Voice mismatch detected!
                        let state = VerificationState::AwaitingName {
                            original_user_id: user_id,
                            original_user_name: name.clone(),
                            voice_signature: Some(sig.clone()),
                        };
                        if let Ok(json) = serde_json::to_string(&state) {
                            self.memory.set_verification_state(conversation_id.0, &json).await;
                        }
                        let reply = format!("Your voice is not matching with {}. What is your name?", name);
                        return Ok(VoiceVerificationOutcome::Intercept(reply));
                    }
                } else {
                    // First time caller with known name speaks with voice signature -> enroll!
                    let _ = self.memory.set_voice_signature(user_id, sig).await;
                }
            }
        }

        Ok(VoiceVerificationOutcome::Continue {
            active_user_id: user_id,
            needs_onboarding: !has_name,
        })
    }

    pub async fn complete(
        &self,
        request: CompleteConversationRequest,
    ) -> Result<(), ConversationError> {
        if request.identity.channel.trim().is_empty()
            || request.identity.external_id.trim().is_empty()
            || request.external_conversation_id.trim().is_empty()
        {
            return Err(ConversationError::Invalid);
        }
        let conversation = sqlx::query(
            "SELECT id FROM conversations WHERE channel = $1 AND external_id = $2",
        )
        .bind(request.identity.channel.trim())
        .bind(request.external_conversation_id.trim())
        .fetch_optional(self.db.pool())
        .await?;

        let conversation_id = match conversation {
            Some(row) => row.get::<Uuid, _>("id"),
            None => return Ok(()),
        };

        let mut tx = self.db.pool().begin().await?;
        sqlx::query(
            "UPDATE conversations SET status = 'completed', completed_at = COALESCE(completed_at, now()) WHERE id = $1",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;

        // Enqueue post-conversation summarization job if not already enqueued
        sqlx::query(
            "INSERT INTO jobs (kind, payload_reference_id) VALUES ('summarize_conversation', $1) \
             ON CONFLICT DO NOTHING",
        )
        .bind(conversation_id)
        .execute(&mut *tx)
        .await?;
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

    async fn resolve_conversation(
        &self,
        user_id: UserId,
        channel: &str,
        external_id: &str,
    ) -> Result<(ConversationId, UserId), ConversationError> {
        let row = sqlx::query(
            "INSERT INTO conversations (user_id, channel, external_id) VALUES ($1, $2, $3) \
             ON CONFLICT (channel, external_id) DO UPDATE SET external_id = EXCLUDED.external_id RETURNING id, user_id",
        )
        .bind(user_id.0)
        .bind(channel.trim())
        .bind(external_id.trim())
        .fetch_one(self.db.pool())
        .await?;
        let stored_user: Uuid = row.get("user_id");
        Ok((ConversationId(row.get("id")), UserId(stored_user)))
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

/// Extracts a user's stated name from an introductory or onboarding message
/// (e.g. "My name is Rahul", "I'm Rahul", "Call me Rahul") to bypass synchronous tool calls.
pub fn extract_name_from_text(text: &str) -> Option<String> {
    let t = text.trim();
    let lower = t.to_ascii_lowercase();

    // Strip common conversational greeting prefixes like "hi", "hello", "hey"
    let mut cleaned_lower = lower.as_str();
    let mut cleaned_orig = t;
    for greeting in &[
        "hi,", "hi", "hello,", "hello", "hey,", "hey",
        "good morning,", "good morning", "good evening,", "good evening", "good afternoon,", "good afternoon"
    ] {
        if let Some(rest) = cleaned_lower.strip_prefix(greeting) {
            let offset = t.len() - rest.trim_start().len();
            cleaned_orig = t[offset..].trim_start();
            cleaned_lower = rest.trim_start();
            break;
        }
    }

    let prefixes = [
        "my name is ",
        "name is ",
        "i am ",
        "i'm ",
        "call me ",
        "this is ",
        "it's ",
        "it is ",
    ];

    for prefix in prefixes {
        if cleaned_lower.starts_with(prefix) {
            let candidate = cleaned_orig[prefix.len()..].trim().trim_end_matches(['.', '!', '?']);
            let word_count = candidate.split_whitespace().count();
            if !candidate.is_empty()
                && word_count >= 1
                && word_count <= 3
                && !candidate.contains(['\n', '\r', '\t', '{', '}', '[', ']'])
            {
                return Some(candidate.to_string());
            }
        }
    }

    // Direct name utterance fallback: if it's 1-2 words and not a generic conversational phrase
    let trimmed = cleaned_orig.trim().trim_end_matches(['.', '!', '?']);
    let words: Vec<&str> = trimmed.split_whitespace().collect();
    if words.len() >= 1 && words.len() <= 2 && trimmed.len() <= 30 {
        if words.iter().all(|w| w.chars().next().map_or(false, |c| c.is_alphabetic())) {
            let lower_single = trimmed.to_ascii_lowercase();
            let non_names = [
                "yes", "no", "nope", "yeah", "yup", "ok", "okay", "sure", "thanks", "thank you",
                "hello", "hi", "bye", "goodbye", "who is this", "what is this", "help", "who are you"
            ];
            if !non_names.contains(&lower_single.as_str()) {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_name_from_text() {
        assert_eq!(extract_name_from_text("My name is Rahul."), Some("Rahul".into()));
        assert_eq!(extract_name_from_text("my name is rahul"), Some("rahul".into()));
        assert_eq!(extract_name_from_text("I'm Rahul Sharma"), Some("Rahul Sharma".into()));
        assert_eq!(extract_name_from_text("Call me John Doe"), Some("John Doe".into()));
        assert_eq!(extract_name_from_text("Hi, my name is Rahul"), Some("Rahul".into()));
        assert_eq!(extract_name_from_text("Hey, I'm Rahul"), Some("Rahul".into()));
        assert_eq!(extract_name_from_text("Rahul"), Some("Rahul".into()));
        assert_eq!(extract_name_from_text("Nope."), None);
        assert_eq!(extract_name_from_text("Hello there"), None);
    }

    #[test]
    fn test_verification_state_serialization() {
        let state = VerificationState::AwaitingName {
            original_user_id: UserId(Uuid::new_v4()),
            original_user_name: "Rahul".into(),
            voice_signature: Some(VoiceSignature::new(vec![0.1, 0.2, 0.3])),
        };
        let serialized = serde_json::to_string(&state).unwrap();
        let deserialized: VerificationState = serde_json::from_str(&serialized).unwrap();
        match deserialized {
            VerificationState::AwaitingName { original_user_name, voice_signature, .. } => {
                assert_eq!(original_user_name, "Rahul");
                assert_eq!(voice_signature.unwrap().features, vec![0.1, 0.2, 0.3]);
            }
            _ => panic!("unexpected state"),
        }
    }
}
