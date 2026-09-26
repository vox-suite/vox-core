/**
* Shared LLM gateway selection: routes through an OpenAI-compatible gateway
* (e.g. TrueFoundry's LLM Gateway) when configured, otherwise calls Gemini
* directly. Both paths produce the same `rig` client capabilities, so every
* existing `.tool(...)` chain keeps working unchanged either way.
*/
use super::AgentError;
use crate::config::Config;
use rig::{
    client::AgentClientExt,
    completion::Prompt,
    providers::{gemini, openai},
};

#[derive(Clone, Debug)]
pub struct LlmGateway {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
}

impl Config {
    pub fn llm_gateway(&self) -> Option<LlmGateway> {
        let base_url = self.llm_gateway_base_url.clone()?;
        let api_key = self.llm_gateway_api_key.clone()?;
        let model = self
            .llm_gateway_model
            .clone()
            .unwrap_or_else(|| self.gemini_model.clone());
        Some(LlmGateway {
            base_url,
            api_key,
            model,
        })
    }
}

/// For agents that need only a bare preamble + prompt, no tools.
pub async fn complete(
    gemini_api_key: &str,
    gemini_model: &str,
    gateway: Option<&LlmGateway>,
    preamble: &str,
    input: String,
) -> Result<String, AgentError> {
    match gateway {
        Some(gateway) => {
            let client = openai::Client::builder()
                .api_key(&gateway.api_key)
                .base_url(&gateway.base_url)
                .build()
                .map_err(|_| AgentError::Provider)?;
            client
                .agent(&gateway.model)
                .preamble(preamble)
                .build()
                .prompt(input)
                .await
                .map_err(|_| AgentError::Provider)
        }
        None => {
            let client = gemini::Client::new(gemini_api_key).map_err(|_| AgentError::Provider)?;
            client
                .agent(gemini_model)
                .preamble(preamble)
                .build()
                .prompt(input)
                .await
                .map_err(|_| AgentError::Provider)
        }
    }
}
