/**
* Conversational agent logic, prompting structures, and TTS token chunking.
*/
use super::{AgentError, tools};
use crate::{
    config::Config,
    db::Db,
    identity::{ResourceOwner, UserId},
    realtime::{DeviceHub, UserEventHub},
};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use tracing::Instrument;

use futures_util::Stream;
use std::pin::Pin;

pub use super::prompts::{
    ELEVENLABS_VOICE_CALL_PREAMBLE, GENERAL_PREAMBLE, OUTBOUND_OPENING_INSTRUCTION,
    VOICE_CALL_PREAMBLE, WHATSAPP_PREAMBLE, is_elevenlabs_provider, is_voice_channel,
    onboarding_instruction, preamble_for_channel, preamble_for_channel_and_tts,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationPrompt {
    #[serde(default)]
    pub correlation: tools::timing::TurnCorrelation,
    pub context: crate::identity::ResolvedUserContext,
    #[serde(skip)]
    pub task_capture: tools::library::TaskCapture,
    pub selected_agent: crate::agent_registry::SelectedAgent,
    pub user_id: UserId,
    #[serde(default)]
    pub user_name: Option<String>,
    pub owner: ResourceOwner,
    pub channel: String,
    pub user_context: String,
    pub recent_messages: Vec<PromptMessage>,
    pub user_text: String,
    pub initiation_context: Option<String>,
    pub needs_onboarding: bool,
    #[serde(default)]
    pub tts_provider: Option<String>,
    #[serde(default)]
    pub filler: Option<String>,
    /// Groups every turn of one call or chat into a Langfuse session.
    #[serde(default)]
    pub conversation_id: Option<uuid::Uuid>,
}

pub struct ConversationAgent {
    api_key: String,
    db: Option<Db>,
    memory: Option<crate::memory::MemoryService>,
    connections: Option<crate::fresh_connections::FreshConnectionsService>,
    tts_provider: String,
    user_events: Option<UserEventHub>,
    device_hub: Option<DeviceHub>,
}

pub type AgentStream = Pin<Box<dyn Stream<Item = Result<String, AgentError>> + Send>>;

#[async_trait]
pub trait ConversationResponder: Send + Sync {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError>;
    async fn respond_stream(&self, prompt: ConversationPrompt) -> Result<AgentStream, AgentError> {
        let text = self.respond(prompt).await?;
        Ok(Box::pin(futures_util::stream::once(
            async move { Ok(text) },
        )))
    }
}

impl ConversationAgent {
    pub fn new(config: &Config) -> Result<Self, AgentError> {
        Ok(Self {
            api_key: config.gemini_api_key.clone(),
            db: None,
            memory: None,
            connections: None,
            tts_provider: crate::config::TTS_PROVIDER.to_string(),
            user_events: None,
            device_hub: None,
        })
    }

    pub fn with_db(config: &Config, db: Db) -> Result<Self, AgentError> {
        let mut agent = Self::new(config)?;
        agent.connections = Some(
            crate::fresh_connections::FreshConnectionsService::new(
                db.pool().clone(),
                config.credential_key.as_deref(),
                None,
                config.google_client_id.clone(),
                config.google_client_secret.clone(),
                config.core_api_url.clone(),
            )
            .map_err(|_| AgentError::Provider)?,
        );
        agent.memory = Some(crate::memory::MemoryService::new(db.clone(), None));
        agent.db = Some(db);
        Ok(agent)
    }

    pub fn with_memory(mut self, memory: crate::memory::MemoryService) -> Self {
        self.memory = Some(memory);
        self
    }

    pub fn with_user_events(mut self, hub: UserEventHub) -> Self {
        self.user_events = Some(hub);
        self
    }

    pub fn with_device_hub(mut self, hub: DeviceHub) -> Self {
        self.device_hub = Some(hub);
        self
    }

    async fn build_agent_and_input(
        &self,
        prompt: &ConversationPrompt,
    ) -> Result<(rig::agent::Agent, String, bool), AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;
        if prompt.selected_agent.model_configuration.model_adapter != "gemini" {
            return Err(AgentError::Provider);
        }

        let is_voice = is_voice_channel(&prompt.channel);
        let tts = prompt.tts_provider.as_deref().unwrap_or(&self.tts_provider);
        let preamble = preamble_for_channel_and_tts(&prompt.channel, Some(tts));
        let is_call_opening =
            is_voice && prompt.initiation_context.is_some() && prompt.recent_messages.is_empty();
        let is_outbound_opening = is_call_opening
            && prompt.channel.trim().eq_ignore_ascii_case("phone")
            && prompt.initiation_context.as_deref()
                != Some("The call just connected. Greet the user.");
        let onboarding_instruction = if is_outbound_opening {
            OUTBOUND_OPENING_INSTRUCTION
        } else {
            onboarding_instruction(&prompt.channel, is_call_opening, prompt.needs_onboarding)
        };

        // Tool exposure is independent of inferred intent and channel. The library
        // rechecks owned-agent access on every operation; loading is not authority.
        let name = &prompt.selected_agent.definition.external_key;
        tracing::Span::current().record("gen_ai.agent.name", name.as_str());
        let agent = client
            .agent(&prompt.selected_agent.model_configuration.model)
            .name(name)
            .preamble(&format!(
                "{preamble}\n{}",
                super::prompts::GOVERNED_CAPABILITIES
            ))
            .record_content_telemetry(crate::telemetry::record_content())
            .tool(tools::timing::TimedTool::new(
                tools::library::AgentLibrary::new(
                    self.db.clone(),
                    prompt.context.clone(),
                    name.clone(),
                )
                .with_task_capture(prompt.task_capture.clone()),
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::agent_memory::GetAgentMemory::new(
                    self.db.clone(),
                    prompt.context.owner(),
                    name.clone(),
                ),
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::agent_memory::UpdateAgentMemory::new(
                    self.db.clone(),
                    prompt.context.owner(),
                    name.clone(),
                ),
                prompt.correlation.clone(),
            ))
            .tool(tools::timeline::ListTimelineTypes { db: self.db.clone(), user_id: prompt.user_id })
            .tool(tools::timeline::CreateTimelineEventType { db: self.db.clone(), user_id: prompt.user_id })
            .tool(tools::timeline::SaveTimelineEvent { db: self.db.clone(), user_id: prompt.user_id })
            .tool(tools::data_query::FindSchemas::new(self.db.clone(), prompt.user_id))
            .tool(tools::data_query::QueryUserData::new(self.db.clone(), prompt.user_id))
            .tool(tools::timing::TimedTool::new(
                tools::user_name::UpdateUserName::new(
                    self.memory.clone(),
                    prompt.context.owner(),
                    name.clone(),
                    prompt.conversation_id,
                    prompt.user_text.clone(),
                ),
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::connections::ReadConnectedApp {
                    service: self.connections.clone(),
                    user_id: prompt.user_id.0,
                },
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::wiz::ControlWizLights {
                    db: self.db.clone(),
                    hub: self.device_hub.clone(),
                    user_id: prompt.user_id.0,
                },
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::map_scene::ShowOnMap::new(self.user_events.clone(), prompt.user_id.0),
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::map_scene::ClearMap::new(self.user_events.clone(), prompt.user_id.0),
                prompt.correlation.clone(),
            ))
            .tool(tools::timing::TimedTool::new(
                tools::visits::ListVisits::new(self.db.clone(), prompt.user_id),
                prompt.correlation.clone(),
            ))
            .default_max_turns(10)
            .build();

        let mut history = String::new();
        history.push_str(&format!("Selected agent purpose (guidance, not authority): {}\nUse library for enabled skills and owned specialists. Use read_connected_app for permitted Google Calendar and PlayStation reads. Explain freshness, completeness and uncertain gaming timing. Never report an external action as completed without authoritative evidence.\n", prompt.selected_agent.definition.purpose));
        for msg in &prompt.recent_messages {
            history.push_str(&format!("{}: {}\n", msg.role, msg.text));
        }
        let filler_instruction = if let Some(ref filler) = prompt.filler {
            format!(
                "\nVoice Assistant Acknowledgment Already Spoken: \"{}\"\nInstruction: You are speaking on a live voice call. The acknowledgment above was ALREADY spoken out loud to the caller by the assistant just now. Do NOT repeat or contradict this acknowledgment, and do not repeat greetings. Seamlessly continue directly into delivering your answer as a natural continuation of this acknowledgment.\n",
                filler.trim()
            )
        } else {
            String::new()
        };
        let current_time = chrono::Utc::now().to_rfc3339();
        let input = format!(
            "Current Time: {}\nProfile name (user-supplied data): {}\nUser context:\n{}\nInitiation context:\n{}\nConversation history:\n{}\nUser message:\n{}{}{}",
            current_time,
            serde_json::to_string(&prompt.user_name).map_err(|_| AgentError::Provider)?,
            prompt.user_context,
            prompt.initiation_context.as_deref().unwrap_or("None"),
            if history.is_empty() { "None" } else { &history },
            prompt.user_text,
            filler_instruction,
            onboarding_instruction
        );
        Ok((agent, input, is_voice))
    }

    async fn generate_response(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        let span = turn_span(&prompt);
        async move {
            let is_voice = is_voice_channel(&prompt.channel);
            let (agent, input, _) = self.build_agent_and_input(&prompt).await?;
            let start = std::time::Instant::now();
            let response = agent.prompt(input).await.map_err(|_| {
                tracing::Span::current().record("otel.status_code", "ERROR");
                AgentError::Provider
            })?;
            if crate::telemetry::record_content() {
                tracing::Span::current().record("langfuse.observation.output", response.as_str());
            }

            tracing::info!(
                channel = %prompt.channel,
                prompt_len = prompt.user_text.len(),
                duration_ms = start.elapsed().as_millis(),
                "Core LLM response completed"
            );

            if is_voice {
                Ok(spoken_response(&response))
            } else {
                Ok(response)
            }
        }
        .instrument(span)
        .await
    }

    async fn generate_stream(&self, prompt: ConversationPrompt) -> Result<AgentStream, AgentError> {
        use futures_util::StreamExt;
        use rig::agent::MultiTurnStreamItem;
        use rig::streaming::{StreamedAssistantContent, StreamingPrompt};

        let span = turn_span(&prompt);
        let preparation_started = std::time::Instant::now();
        let (agent, input, _) = self
            .build_agent_and_input(&prompt)
            .instrument(span.clone())
            .await?;
        tracing::info!(
            conversation_id = prompt.correlation.conversation_id.as_deref(), turn_id = prompt.correlation.turn_id.as_deref(), revision = prompt.correlation.revision,
            model = %prompt.selected_agent.model_configuration.model, agent = %prompt.selected_agent.definition.external_key,
            history_messages = prompt.recent_messages.len(), history_bytes = prompt.recent_messages.iter().map(|m| m.text.len()).sum::<usize>(), context_bytes = prompt.user_context.len(),
            channel = %prompt.channel,
            preparation_ms = preparation_started.elapsed().as_millis(),
            "CORE_AGENT_PREPARATION"
        );
        let provider_started = std::time::Instant::now();
        let stream = async move { agent.stream_prompt(input).await }
            .instrument(span.clone())
            .await;

        let model_correlation = prompt.correlation.clone();
        let text_stream = stream.filter_map(move |item_res| {
            let correlation = model_correlation.clone();
            async move {
            match item_res {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                    if !t.text.is_empty() {
                        Some(Ok(t.text))
                    } else {
                        None
                    }
                }
                Ok(MultiTurnStreamItem::CompletionCall(call)) => {
                    let usage_known = call.usage.total_tokens > 0 || call.usage.input_tokens > 0 || call.usage.output_tokens > 0;
                    tracing::info!(conversation_id = correlation.conversation_id.as_deref(), turn_id = correlation.turn_id.as_deref(), revision = correlation.revision,
                        model_round = call.call_index, usage_known,
                        input_tokens = usage_known.then_some(call.usage.input_tokens), output_tokens = usage_known.then_some(call.usage.output_tokens), reasoning_tokens = usage_known.then_some(call.usage.reasoning_tokens),
                        cached_input_tokens = usage_known.then_some(call.usage.cached_input_tokens), finish_reason = ?call.finish_reason,
                        "CORE_MODEL_ROUND_FINISHED");
                    None
                }
                Ok(_) => None,
                Err(err) => {
                    let _ = err;
                    tracing::error!("CORE_MODEL_STREAM_FAILED");
                    Some(Err(AgentError::Provider))
                }
            }
        }});

        Ok(traced_reply(
            text_stream,
            span,
            crate::telemetry::record_content(),
            prompt.correlation,
            provider_started,
        ))
    }
}

/// Polls the reply inside the turn span, since the caller polls it after
/// `generate_stream` returns, so rig's chat and tool spans nest under the turn.
fn traced_reply(
    reply: impl Stream<Item = Result<String, AgentError>> + Send + 'static,
    span: tracing::Span,
    record_content: bool,
    correlation: tools::timing::TurnCorrelation,
    started: std::time::Instant,
) -> AgentStream {
    use futures_util::StreamExt;

    let spoken = SpokenReply {
        span,
        text: String::new(),
        record: record_content,
        correlation,
        started,
        first_text_ms: None,
        outcome: "cancelled",
    };
    Box::pin(futures_util::stream::unfold(
        (Box::pin(reply), spoken),
        |(mut stream, mut spoken)| async move {
            let Some(item) = stream.next().instrument(spoken.span.clone()).await else {
                if spoken.outcome != "failed" {
                    spoken.outcome = "completed";
                }
                drop(spoken);
                return None;
            };
            match &item {
                Ok(text) => {
                    if spoken.first_text_ms.is_none() && !text.is_empty() {
                        let ms = spoken.started.elapsed().as_millis() as u64;
                        spoken.first_text_ms = Some(ms);
                        tracing::info!(
                            conversation_id = spoken.correlation.conversation_id.as_deref(),
                            turn_id = spoken.correlation.turn_id.as_deref(),
                            revision = spoken.correlation.revision,
                            model_first_text_ms = ms,
                            "CORE_MODEL_FIRST_TEXT"
                        );
                    }
                    spoken.text.push_str(text);
                }
                Err(_) => {
                    spoken.outcome = "failed";
                    spoken.span.record("otel.status_code", "ERROR");
                }
            }
            Some((item, (stream, spoken)))
        },
    ))
}

/// Records the agent's reply on the turn span once, when the reply ends or the
/// caller drops it mid-turn (barge-in), so a cut-off turn keeps what was said.
struct SpokenReply {
    span: tracing::Span,
    text: String,
    record: bool,
    correlation: tools::timing::TurnCorrelation,
    started: std::time::Instant,
    first_text_ms: Option<u64>,
    outcome: &'static str,
}

impl Drop for SpokenReply {
    fn drop(&mut self) {
        tracing::info!(
            conversation_id = self.correlation.conversation_id.as_deref(),
            turn_id = self.correlation.turn_id.as_deref(),
            revision = self.correlation.revision,
            outcome = self.outcome,
            model_first_text_ms = self.first_text_ms,
            model_stream_ms = self.started.elapsed().as_millis() as u64,
            reply_bytes = self.text.len(),
            "CORE_MODEL_FINISHED"
        );
        if self.record && !self.text.is_empty() {
            self.span
                .record("langfuse.observation.output", self.text.as_str());
        }
    }
}

/// Root span of one agent turn: one Langfuse trace, grouped per conversation,
/// exported under the name of the agent that ran (`shopping-agent`, ...).
fn turn_span(prompt: &ConversationPrompt) -> tracing::Span {
    tracing::info_span!(
        target: crate::telemetry::TURN_TARGET,
        "conversation_turn",
        gen_ai.agent.name = tracing::field::Empty,
        langfuse.observation.type = "agent",
        session.id = prompt.conversation_id.map(tracing::field::display),
        user.id = %prompt.user_id.0,
        langfuse.trace.tags = %serde_json::json!([prompt.channel]),
        channel = %prompt.channel,
        vox.tool_domain = tracing::field::Empty,
        otel.status_code = tracing::field::Empty,
        // The caller's words and the agent's reply, only with content tracing on.
        langfuse.observation.input = crate::telemetry::record_content()
            .then_some(prompt.user_text.as_str()),
        langfuse.observation.output = tracing::field::Empty,
    )
}

pub fn spoken_response(response: &str) -> String {
    let mut spoken = Vec::new();
    for line in response.lines() {
        let mut line = line.trim();
        if line.starts_with("```") {
            continue;
        }
        line = line.trim_start_matches('#').trim_start();
        for marker in ["- ", "* ", "+ "] {
            if let Some(value) = line.strip_prefix(marker) {
                line = value;
                break;
            }
        }
        if let Some((number, value)) = line.split_once(". ")
            && !number.is_empty()
            && number.chars().all(|character| character.is_ascii_digit())
        {
            line = value;
        }
        let line = remove_markdown_links(line);
        let line = line.replace(['*', '`', '_'], "");
        spoken.extend(
            line.split_whitespace()
                .filter(|word| !is_url(word))
                .map(str::to_owned),
        );
    }
    let combined = spoken.join(" ");
    let with_currency = normalize_currency(&combined);
    expand_abbreviations(&with_currency)
}

fn normalize_currency(text: &str) -> String {
    let mut words = Vec::new();
    for token in text.split_whitespace() {
        let clean = token.trim_end_matches([',', '.', '!', '?', ';', ':']);
        let punctuation = &token[clean.len()..];

        if let Some(stripped) = clean.strip_prefix('$') {
            if let Some((dollars, cents)) = stripped.split_once('.') {
                if dollars.chars().all(|c| c.is_ascii_digit())
                    && cents.chars().all(|c| c.is_ascii_digit())
                    && !dollars.is_empty()
                    && !cents.is_empty()
                {
                    words.push(format!("{dollars} dollars and {cents} cents{punctuation}"));
                    continue;
                }
            } else if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
                words.push(format!("{stripped} dollars{punctuation}"));
                continue;
            }
        } else if let Some(stripped) = clean.strip_prefix('₹')
            && stripped.chars().all(|c| c.is_ascii_digit())
            && !stripped.is_empty()
        {
            words.push(format!("{stripped} rupees{punctuation}"));
            continue;
        }
        words.push(token.to_string());
    }
    words.join(" ")
}

fn expand_abbreviations(text: &str) -> String {
    let mut words = Vec::new();
    for token in text.split_whitespace() {
        let clean = token.trim_end_matches([',', ';', ':']);
        let punctuation = &token[clean.len()..];
        let expanded = match clean {
            "vs." | "Vs." => "versus",
            "Jan." => "January",
            "Feb." => "February",
            "Mar." => "March",
            "Apr." => "April",
            "Aug." => "August",
            "Sept." => "September",
            "Oct." => "October",
            "Nov." => "November",
            "Dec." => "December",
            "Dr." => "Doctor",
            "Mr." => "Mister",
            "Mrs." => "Missus",
            "approx." => "approximately",
            _ => clean,
        };
        if expanded != clean {
            words.push(format!("{expanded}{punctuation}"));
        } else {
            words.push(token.to_string());
        }
    }
    words.join(" ")
}

pub fn split_spoken_sentences(text: &str) -> Vec<String> {
    let mut sentences = Vec::new();
    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut start = 0;

    let mut i = 0;
    while i < len {
        let c = chars[i];
        if c == '.' || c == '!' || c == '?' {
            let next_is_boundary = i + 1 == len || chars[i + 1].is_whitespace();

            if next_is_boundary {
                let is_decimal = if c == '.' && i > 0 && i + 1 < len {
                    chars[i - 1].is_ascii_digit() && chars[i + 1].is_ascii_digit()
                } else {
                    false
                };

                let prefix: String = chars[start..i].iter().collect();
                let last_word = prefix.split_whitespace().last().unwrap_or("");
                let is_abbr = matches!(
                    last_word.to_ascii_lowercase().as_str(),
                    "mr" | "mrs"
                        | "ms"
                        | "dr"
                        | "prof"
                        | "sr"
                        | "jr"
                        | "vs"
                        | "eg"
                        | "ie"
                        | "etc"
                        | "st"
                        | "ave"
                        | "oct"
                        | "nov"
                        | "dec"
                        | "jan"
                        | "feb"
                        | "mar"
                        | "apr"
                        | "aug"
                        | "sept"
                );

                if !is_decimal && !is_abbr {
                    let candidate: String = chars[start..=i].iter().collect();
                    let trimmed = candidate.trim().to_string();
                    if trimmed.chars().any(|ch| ch.is_alphabetic()) {
                        sentences.push(trimmed);
                        start = i + 1;
                    }
                }
            }
        }
        i += 1;
    }

    if start < len {
        let remaining: String = chars[start..].iter().collect();
        let trimmed = remaining.trim().to_string();
        if trimmed.chars().any(|ch| ch.is_alphabetic()) {
            sentences.push(trimmed);
        } else if !trimmed.is_empty() && !sentences.is_empty() {
            let last_idx = sentences.len() - 1;
            sentences[last_idx].push(' ');
            sentences[last_idx].push_str(&trimmed);
        }
    }

    if sentences.is_empty() && text.trim().chars().any(|ch| ch.is_alphabetic()) {
        sentences.push(text.trim().to_string());
    }

    sentences
}

fn remove_markdown_links(value: &str) -> String {
    let mut output = String::new();
    let mut rest = value;
    while let Some(open) = rest.find('[') {
        output.push_str(&rest[..open]);
        let after_open = &rest[open + 1..];
        let Some(close_label) = after_open.find("](") else {
            output.push_str(&rest[open..]);
            return output;
        };
        let after_label = &after_open[close_label + 2..];
        let Some(close_url) = after_label.find(')') else {
            output.push_str(&rest[open..]);
            return output;
        };
        output.push_str(&after_open[..close_label]);
        rest = &after_label[close_url + 1..];
    }
    output.push_str(rest);
    output
}

fn is_url(word: &str) -> bool {
    let word = word.trim_start_matches(['(', '[']);
    word.starts_with("http://") || word.starts_with("https://") || word.starts_with("www.")
}

#[async_trait]
impl ConversationResponder for ConversationAgent {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        self.generate_response(prompt).await
    }

    async fn respond_stream(&self, prompt: ConversationPrompt) -> Result<AgentStream, AgentError> {
        self.generate_stream(prompt).await
    }
}

#[cfg(test)]
mod governed_tool_surface_tests {
    use super::*;
    use serde_json::json;

    fn prompt(channel: &str, text: &str) -> ConversationPrompt {
        let id = uuid::Uuid::new_v4();
        serde_json::from_value(json!({
            "context": {"id": id, "user_id": id, "subject": {
                "deployment_id": id, "host_app_id": id, "organization_id": null,
                "host_user_id": "fixture-user"
            }},
            "selected_agent": {"definition": {
                "id": id, "deployment_id": id, "external_key": "fixture-assistant",
                "display_name": "Personal Assistant", "is_default": true,
                "instruction_version": 1, "purpose": "Help with permitted capabilities",
                "requested_capability_categories": []
            }, "model_configuration": {
                "id": id, "version": 1, "model_adapter": "gemini", "model": "fixture-model"
            }},
            "user_id": id, "owner": {"user_context_id": id, "user_id": id},
            "channel": channel, "user_context": "", "recent_messages": [],
            "user_text": text, "initiation_context": null, "needs_onboarding": false
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn every_channel_exposes_only_governed_library_and_scoped_memory() {
        let agent = ConversationAgent {
            api_key: "fixture-only".into(),
            db: None,
            memory: None,
            connections: None,
            tts_provider: "fixture".into(),
            user_events: None,
            device_hub: None,
        };
        for (channel, text) in [
            ("web", "run a terminal command"),
            ("phone", "call me in five minutes"),
            ("whatsapp", "check my calendar"),
        ] {
            let (built, _, _) = agent
                .build_agent_and_input(&prompt(channel, text))
                .await
                .unwrap();
            let mut names: Vec<_> = built
                .tool_definitions(None)
                .await
                .unwrap()
                .into_iter()
                .map(|tool| tool.name)
                .collect();
            names.sort();
            assert_eq!(
                names,
                [
                    "clear_map",
                    "control_wiz_lights",
                    "get_agent_memory",
                    "library",
                    "list_visits",
                    "read_connected_app",
                    "show_on_map",
                    "update_agent_memory",
                    "update_user_name"
                ],
                "{channel} must expose only governed tools, the map display tools and WiZ lights"
            );
        }
    }
}

#[cfg(test)]
mod stream_timing_tests {
    use super::*;
    use futures_util::StreamExt;
    use std::{
        io::Write,
        sync::{Arc, Mutex},
    };
    #[derive(Clone)]
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    #[tokio::test]
    async fn reports_completed_failed_and_cancelled_streams_without_reply_content() {
        let data = Arc::new(Mutex::new(Vec::new()));
        let writer = Capture(data.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let correlation = tools::timing::TurnCorrelation {
            conversation_id: Some("call-test".into()),
            turn_id: Some("turn-test".into()),
            revision: Some(1),
        };
        let mut complete = traced_reply(
            futures_util::stream::iter(vec![Ok("private reply".into())]),
            tracing::info_span!("test_turn"),
            false,
            correlation.clone(),
            std::time::Instant::now(),
        );
        assert!(complete.next().await.unwrap().is_ok());
        assert!(complete.next().await.is_none());
        let mut failed = traced_reply(
            futures_util::stream::iter(vec![Err(AgentError::Provider)]),
            tracing::info_span!("test_turn"),
            false,
            correlation.clone(),
            std::time::Instant::now(),
        );
        assert!(failed.next().await.unwrap().is_err());
        drop(failed);
        let cancelled = traced_reply(
            futures_util::stream::pending(),
            tracing::info_span!("test_turn"),
            false,
            correlation,
            std::time::Instant::now(),
        );
        drop(cancelled);
        let logs = String::from_utf8(data.lock().unwrap().clone()).unwrap();
        for outcome in ["completed", "failed", "cancelled"] {
            assert!(logs.contains(&format!("outcome=\"{outcome}\"")), "{logs}");
        }
        assert!(logs.contains("CORE_MODEL_FIRST_TEXT"));
        assert!(logs.contains("call-test"));
        assert!(logs.contains("turn-test"));
        assert!(!logs.contains("private reply"));
    }
}
