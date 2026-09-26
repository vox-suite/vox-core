/**
* Conversational agent logic, prompting structures, and TTS token chunking.
*/
use super::{AgentError, tools};
use crate::outbound::OutboundCallService;
use crate::realtime::{DeviceHub, UserEventHub};
use crate::{
    config::Config,
    db::Db,
    identity::{ResourceOwner, UserId},
};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use futures_util::Stream;
use std::pin::Pin;

pub use super::prompts::{
    ELEVENLABS_VOICE_CALL_PREAMBLE, GENERAL_PREAMBLE, VOICE_CALL_PREAMBLE, WHATSAPP_PREAMBLE,
    is_elevenlabs_provider, is_voice_channel, onboarding_instruction, preamble_for_channel,
    preamble_for_channel_and_tts,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationPrompt {
    pub user_id: UserId,
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
}

pub struct ConversationAgent {
    api_key: String,
    model: String,
    http: reqwest::Client,
    exa_api_key: String,
    google_maps_api_key: Option<String>,
    db: Option<Db>,
    outbound: Option<Arc<OutboundCallService>>,
    tool_router: Option<crate::jev::ToolRouter>,
    tts_provider: String,
    device_hub: Option<DeviceHub>,
    user_events: Option<UserEventHub>,
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
        let dependencies =
            tools::dependencies::ToolDependencies::new().map_err(|_| AgentError::Provider)?;
        let tool_router = if let Some(ref api_key) = config.jev_api_key {
            let client =
                crate::jev::JevClient::new(api_key.clone(), Some(config.jev_base_url.clone()));
            Some(crate::jev::ToolRouter::new(client))
        } else {
            None
        };
        Ok(Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
            http: dependencies.http,
            exa_api_key: config.exa_api_key.clone(),
            google_maps_api_key: config.google_maps_api_key.clone(),
            db: None,
            outbound: None,
            tool_router,
            tts_provider: config.tts_provider.clone(),
            device_hub: None,
            user_events: None,
        })
    }

    pub fn with_db(config: &Config, db: Db) -> Result<Self, AgentError> {
        let mut agent = Self::new(config)?;
        let bridge_client = config.bridge_url.as_ref().and_then(|url| {
            crate::bridge_client::BridgeClient::new(url.clone(), config.service_token.clone())
                .ok()
                .map(|c| Arc::new(c) as Arc<dyn crate::bridge_client::OutboundBridge>)
        });
        agent.outbound = Some(Arc::new(OutboundCallService::new(
            db.clone(),
            bridge_client,
        )));
        agent.db = Some(db);
        Ok(agent)
    }

    pub fn with_outbound(mut self, outbound: Arc<OutboundCallService>) -> Self {
        self.outbound = Some(outbound);
        self
    }

    pub fn with_tool_router(mut self, router: crate::jev::ToolRouter) -> Self {
        self.tool_router = Some(router);
        self
    }

    pub fn with_device_hub(mut self, hub: DeviceHub) -> Self {
        self.device_hub = Some(hub);
        self
    }

    pub fn with_user_events(mut self, hub: UserEventHub) -> Self {
        self.user_events = Some(hub);
        self
    }

    async fn build_agent_and_input(
        &self,
        prompt: &ConversationPrompt,
    ) -> Result<(rig::agent::Agent, String, bool), AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;

        let is_voice = is_voice_channel(&prompt.channel);
        let tts = prompt.tts_provider.as_deref().unwrap_or(&self.tts_provider);
        let preamble = preamble_for_channel_and_tts(&prompt.channel, Some(tts));
        let is_call_opening =
            is_voice && prompt.initiation_context.is_some() && prompt.recent_messages.is_empty();
        let onboarding_instruction =
            onboarding_instruction(&prompt.channel, is_call_opening, prompt.needs_onboarding);

        // ponytail: fixed 0.55 cutoff, tune from routed-domain/confidence logs if it misfires elsewhere
        const TOOL_DOMAIN_CONFIDENCE_THRESHOLD: f64 = 0.55;
        // One id per agent turn: device commands proposed in this turn can
        // only be confirmed by a later one (see RunTerminalCommand).
        let turn = uuid::Uuid::new_v4();

        let awaiting_device_confirmation = self
            .device_hub
            .as_ref()
            .is_some_and(|hub| hub.has_pending_command(prompt.user_id.0));
        let routed_domain = if is_call_opening {
            crate::jev::ToolDomain::None
        } else if awaiting_device_confirmation {
            // A bare "yes" would otherwise route to no tools and the pending
            // device command could never be confirmed.
            crate::jev::ToolDomain::Device
        } else if let Some(router) = &self.tool_router {
            match router.classify(&prompt.user_text).await {
                Ok((domain, confidence)) if confidence >= TOOL_DOMAIN_CONFIDENCE_THRESHOLD => {
                    domain
                }
                Ok((domain, confidence)) => {
                    tracing::warn!(
                        ?domain,
                        confidence,
                        "Jev tool router confidence too low for a single domain, falling back to all tools"
                    );
                    crate::jev::ToolDomain::All
                }
                Err(err) => {
                    tracing::warn!(%err, "Jev tool router classification failed, falling back to all tools");
                    crate::jev::ToolDomain::All
                }
            }
        } else {
            crate::jev::ToolDomain::All
        };

        let agent =
            if is_call_opening || (is_voice && routed_domain == crate::jev::ToolDomain::None) {
                client
                    .agent(&self.model)
                    .preamble(preamble)
                    .tool(tools::profile::UpdateUserInfo::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .default_max_turns(2)
                    .build()
            } else if is_voice {
                match routed_domain {
                    crate::jev::ToolDomain::WebSearch => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::web_search::WebSearch::new(
                            self.http.clone(),
                            self.exa_api_key.clone(),
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                    crate::jev::ToolDomain::Maps => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::google_maps::SearchPlaces::new(
                            self.http.clone(),
                            self.google_maps_api_key.clone(),
                        ))
                        .tool(tools::google_maps::GetRoute::new(
                            self.http.clone(),
                            self.google_maps_api_key.clone(),
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                    crate::jev::ToolDomain::TasksAndRecords => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::spans::CreateSpan::new(
                            self.db.clone(),
                            prompt.owner,
                            self.user_events.clone().unwrap_or_default(),
                        ))
                        .tool(tools::spans::ListSpans::new(self.db.clone(), prompt.owner))
                        .tool(tools::spans::GetSpan::new(self.db.clone(), prompt.owner))
                        .tool(tools::spans::UpdateSpan::new(
                            self.db.clone(),
                            prompt.owner,
                            self.user_events.clone().unwrap_or_default(),
                        ))
                        .tool(tools::collections::CreateCollection::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::collections::ListCollections::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::calls::ScheduleOutboundCall::new(
                            self.db.clone(),
                            self.outbound.clone(),
                            prompt.owner,
                        ))
                        .tool(tools::calls::TriggerOutboundCall::new(
                            self.db.clone(),
                            self.outbound.clone(),
                            prompt.owner,
                        ))
                        .tool(tools::records::CreateUserRecord::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::records::ListUserRecords::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::profile::GetUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                    crate::jev::ToolDomain::Calendar => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::spans::ListSpans::new(self.db.clone(), prompt.owner))
                        .tool(tools::profile::GetUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                    crate::jev::ToolDomain::Device => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::terminal::OpenTerminal::new(
                            self.db.clone(),
                            prompt.user_id,
                            self.device_hub.clone().unwrap_or_default(),
                        ))
                        .tool(tools::terminal::RunTerminalCommand::new(
                            self.db.clone(),
                            prompt.user_id,
                            self.device_hub.clone().unwrap_or_default(),
                            turn,
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                    _ => client
                        .agent(&self.model)
                        .preamble(preamble)
                        .tool(tools::web_search::WebSearch::new(
                            self.http.clone(),
                            self.exa_api_key.clone(),
                        ))
                        .tool(tools::google_maps::SearchPlaces::new(
                            self.http.clone(),
                            self.google_maps_api_key.clone(),
                        ))
                        .tool(tools::google_maps::GetRoute::new(
                            self.http.clone(),
                            self.google_maps_api_key.clone(),
                        ))
                        .tool(tools::profile::GetUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::profile::UpdateUserInfo::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::spans::CreateSpan::new(
                            self.db.clone(),
                            prompt.owner,
                            self.user_events.clone().unwrap_or_default(),
                        ))
                        .tool(tools::spans::ListSpans::new(self.db.clone(), prompt.owner))
                        .tool(tools::calls::ScheduleOutboundCall::new(
                            self.db.clone(),
                            self.outbound.clone(),
                            prompt.owner,
                        ))
                        .tool(tools::calls::TriggerOutboundCall::new(
                            self.db.clone(),
                            self.outbound.clone(),
                            prompt.owner,
                        ))
                        .tool(tools::records::CreateUserRecord::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .tool(tools::records::ListUserRecords::new(
                            self.db.clone(),
                            prompt.user_id,
                        ))
                        .default_max_turns(6)
                        .build(),
                }
            } else {
                client
                    .agent(&self.model)
                    .preamble(preamble)
                    .tool(tools::web_search::WebSearch::new(
                        self.http.clone(),
                        self.exa_api_key.clone(),
                    ))
                    .tool(tools::google_maps::SearchPlaces::new(
                        self.http.clone(),
                        self.google_maps_api_key.clone(),
                    ))
                    .tool(tools::google_maps::GetRoute::new(
                        self.http.clone(),
                        self.google_maps_api_key.clone(),
                    ))
                    .tool(tools::profile::GetUserInfo::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::profile::UpdateUserInfo::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::collections::CreateCollection::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::collections::ListCollections::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::collections::GetCollection::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::collections::UpdateCollection::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::spans::CreateSpan::new(
                        self.db.clone(),
                        prompt.owner,
                        self.user_events.clone().unwrap_or_default(),
                    ))
                    .tool(tools::spans::ListSpans::new(self.db.clone(), prompt.owner))
                    .tool(tools::spans::GetSpan::new(self.db.clone(), prompt.owner))
                    .tool(tools::spans::UpdateSpan::new(
                        self.db.clone(),
                        prompt.owner,
                        self.user_events.clone().unwrap_or_default(),
                    ))
                    .tool(tools::calls::ScheduleOutboundCall::new(
                        self.db.clone(),
                        self.outbound.clone(),
                        prompt.owner,
                    ))
                    .tool(tools::calls::TriggerOutboundCall::new(
                        self.db.clone(),
                        self.outbound.clone(),
                        prompt.owner,
                    ))
                    .tool(tools::records::DefineDataSchema::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::records::ListDataSchemas::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::records::CreateUserRecord::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::records::ListUserRecords::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::records::ManageUserGoal::new(
                        self.db.clone(),
                        prompt.user_id,
                    ))
                    .tool(tools::terminal::OpenTerminal::new(
                        self.db.clone(),
                        prompt.user_id,
                        self.device_hub.clone().unwrap_or_default(),
                    ))
                    .tool(tools::terminal::RunTerminalCommand::new(
                        self.db.clone(),
                        prompt.user_id,
                        self.device_hub.clone().unwrap_or_default(),
                        turn,
                    ))
                    .default_max_turns(10)
                    .build()
            };

        let mut history = String::new();
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
            "Current Time: {}\nUser context:\n{}\nInitiation context:\n{}\nConversation history:\n{}\nUser message:\n{}{}{}",
            current_time,
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
        let is_voice = is_voice_channel(&prompt.channel);
        let (agent, input, _) = self.build_agent_and_input(&prompt).await?;
        let start = std::time::Instant::now();
        let response = agent
            .prompt(input)
            .await
            .map_err(|_| AgentError::Provider)?;

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

    async fn generate_stream(&self, prompt: ConversationPrompt) -> Result<AgentStream, AgentError> {
        use futures_util::StreamExt;
        use rig::agent::MultiTurnStreamItem;
        use rig::streaming::{StreamedAssistantContent, StreamingPrompt};

        let preparation_started = std::time::Instant::now();
        let (agent, input, _) = self.build_agent_and_input(&prompt).await?;
        tracing::info!(
            channel = %prompt.channel,
            preparation_ms = preparation_started.elapsed().as_millis(),
            "CORE_AGENT_PREPARATION"
        );
        let stream = agent.stream_prompt(input).await;

        let text_stream = stream.filter_map(|item_res| async move {
            match item_res {
                Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(t))) => {
                    if !t.text.is_empty() {
                        Some(Ok(t.text))
                    } else {
                        None
                    }
                }
                Ok(_) => None,
                Err(err) => {
                    tracing::error!(%err, "Gemini stream error");
                    Some(Err(AgentError::Provider))
                }
            }
        });

        Ok(Box::pin(text_stream))
    }
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
#[path = "../../tests/unit/agents_conversation.rs"]
mod tests;
