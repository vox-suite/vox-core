use super::{AgentError, prompts::*, tools};
use crate::{config::Config, db::Db, identity::UserId};
use async_trait::async_trait;
use rig::{client::AgentClientExt, completion::Prompt, providers::gemini};
use serde::{Deserialize, Serialize};

use futures_util::Stream;
use std::pin::Pin;

pub use super::prompts::{
    GENERAL_PREAMBLE, VOICE_CALL_PREAMBLE, WHATSAPP_PREAMBLE, is_voice_channel,
    onboarding_instruction, preamble_for_channel,
};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptMessage {
    pub role: String,
    pub text: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ConversationPrompt {
    pub user_id: UserId,
    pub channel: String,
    pub user_context: String,
    pub recent_messages: Vec<PromptMessage>,
    pub user_text: String,
    pub initiation_context: Option<String>,
    pub needs_onboarding: bool,
}

pub struct ConversationAgent {
    api_key: String,
    model: String,
    http: reqwest::Client,
    exa_api_key: String,
    google_maps_api_key: Option<String>,
    db: Option<Db>,
}

pub type AgentStream = Pin<Box<dyn Stream<Item = Result<String, AgentError>> + Send>>;

#[async_trait]
pub trait ConversationResponder: Send + Sync {
    async fn respond(&self, prompt: ConversationPrompt) -> Result<String, AgentError>;
    async fn respond_stream(
        &self,
        prompt: ConversationPrompt,
    ) -> Result<AgentStream, AgentError> {
        let text = self.respond(prompt).await?;
        Ok(Box::pin(futures_util::stream::once(async move { Ok(text) })))
    }
}

impl ConversationAgent {
    pub fn new(config: &Config) -> Result<Self, AgentError> {
        let dependencies =
            tools::dependencies::ToolDependencies::new().map_err(|_| AgentError::Provider)?;
        Ok(Self {
            api_key: config.gemini_api_key.clone(),
            model: config.gemini_model.clone(),
            http: dependencies.http,
            exa_api_key: config.exa_api_key.clone(),
            google_maps_api_key: config.google_maps_api_key.clone(),
            db: None,
        })
    }

    pub fn with_db(config: &Config, db: Db) -> Result<Self, AgentError> {
        let mut agent = Self::new(config)?;
        agent.db = Some(db);
        Ok(agent)
    }

    async fn generate_response(&self, prompt: ConversationPrompt) -> Result<String, AgentError> {
        let client = gemini::Client::new(&self.api_key).map_err(|_| AgentError::Provider)?;

        let is_voice = is_voice_channel(&prompt.channel);
        let preamble = preamble_for_channel(&prompt.channel);
        let is_call_opening = is_voice && prompt.initiation_context.is_some() && prompt.recent_messages.is_empty();
        let onboarding_instruction = onboarding_instruction(
            &prompt.channel,
            is_call_opening,
            prompt.needs_onboarding,
        );

        let agent = if is_call_opening {
            // Call opening fast-path: greeting does not require tools.
            // Eliminates tool declarations, reducing TTFT from ~3.5s to <800ms.
            client
                .agent(&self.model)
                .preamble(preamble)
                .default_max_turns(2)
                .build()
        } else if is_voice {
            // Voice channel: only include tools relevant to spoken telephone interactions.
            // Excluding administrative tools (schemas, projects, goals) significantly reduces
            // prompt size and Gemini tool evaluation latency.
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
                .tool(tools::tasks::CreateTask::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::tasks::ListTasks::new(
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
                .tool(tools::devices::DispatchDeviceCommand::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::calls::TriggerOutboundCall::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .default_max_turns(6)
                .build()
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
                .tool(tools::projects::CreateProject::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::projects::ListProjects::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::projects::GetProject::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::projects::UpdateProject::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::tasks::CreateTask::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::tasks::ListTasks::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::tasks::GetTask::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::tasks::UpdateTask::new(
                    self.db.clone(),
                    prompt.user_id,
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
                .tool(tools::devices::ListDevices::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::devices::DispatchDeviceCommand::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .tool(tools::calls::TriggerOutboundCall::new(
                    self.db.clone(),
                    prompt.user_id,
                ))
                .default_max_turns(10)
                .build()
        };

        let mut history = String::new();
        for msg in &prompt.recent_messages {
            history.push_str(&format!("{}: {}\n", msg.role, msg.text));
        }
        let input = format!(
            "User context:\n{}\nInitiation context:\n{}\nConversation history:\n{}\nUser message:\n{}{}",
            prompt.user_context,
            prompt.initiation_context.as_deref().unwrap_or("None"),
            if history.is_empty() { "None" } else { &history },
            prompt.user_text,
            onboarding_instruction
        );
        let start = std::time::Instant::now();
        let response = agent
            .prompt(input)
            .await
            .map_err(|_| AgentError::Provider)?;

        tracing::info!(
            channel = %prompt.channel,
            prompt = %prompt.user_text,
            duration_ms = start.elapsed().as_millis(),
            "Core LLM response completed"
        );

        if is_voice {
            Ok(spoken_response(&response))
        } else {
            Ok(response)
        }
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
        } else if let Some(stripped) = clean.strip_prefix('₹') {
            if stripped.chars().all(|c| c.is_ascii_digit()) && !stripped.is_empty() {
                words.push(format!("{stripped} rupees{punctuation}"));
                continue;
            }
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
                    "mr" | "mrs" | "ms" | "dr" | "prof" | "sr" | "jr" | "vs" | "eg" | "ie" | "etc" | "st" | "ave" | "oct" | "nov" | "dec" | "jan" | "feb" | "mar" | "apr" | "aug" | "sept"
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

    async fn respond_stream(
        &self,
        prompt: ConversationPrompt,
    ) -> Result<AgentStream, AgentError> {
        let is_voice = is_voice_channel(&prompt.channel);
        let text = self.generate_response(prompt).await?;
        if is_voice {
            let sentences = split_spoken_sentences(&text);
            let chunks: Vec<Result<String, AgentError>> = sentences
                .into_iter()
                .map(|s| Ok(format!("{s} ")))
                .collect();
            Ok(Box::pin(futures_util::stream::iter(chunks)))
        } else {
            Ok(Box::pin(futures_util::stream::once(async move { Ok(text) })))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GENERAL_PREAMBLE, VOICE_CALL_PREAMBLE, is_voice_channel, spoken_response,
    };

    #[test]
    fn prompt_requires_natural_speech_only() {
        for requirement in [
            "speaking live",
            "Never use Markdown",
            "URLs",
            "one to three short sentences",
        ] {
            assert!(VOICE_CALL_PREAMBLE.contains(requirement));
        }
    }

    #[test]
    fn normalizes_written_formatting_for_tts() {
        assert_eq!(
            spoken_response(
                "## Update\n1. **Traffic** is heavy.\n2. https://example.com See [the map](https://example.com)."
            ),
            "Update Traffic is heavy. See the map."
        );
    }

    #[test]
    fn preserves_natural_conversational_speech() {
        assert_eq!(
            spoken_response("It looks busy near your office, so I'd leave ten minutes early."),
            "It looks busy near your office, so I'd leave ten minutes early."
        );
    }

    #[test]
    fn normalizes_currency_and_abbreviations_for_speech() {
        assert_eq!(
            spoken_response("Apple stock is currently at $235.40. India vs. West Indies on Oct. 2."),
            "Apple stock is currently at 235 dollars and 40 cents. India versus West Indies on October 2."
        );
    }

    #[test]
    fn splits_spoken_sentences_without_breaking_decimals_or_producing_letterless_chunks() {
        use super::split_spoken_sentences;

        let sentences = split_spoken_sentences(
            "Apple is at $235.40 right now. India plays on Oct. 2, 2025. That is great!",
        );
        assert_eq!(
            sentences,
            vec![
                "Apple is at $235.40 right now.",
                "India plays on Oct. 2, 2025.",
                "That is great!"
            ]
        );
        for s in sentences {
            assert!(s.chars().any(|c| c.is_alphabetic()));
        }
    }
}
