/**
 * ElevenLabs streaming Text-to-Dialogue client delivering raw high-fidelity MP3 chunks.
 */
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use serde::Serialize;
use std::pin::Pin;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct ElevenLabsClient {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
    model_id: String,
    voice_id: String,
    output_format: String,
}

#[derive(Serialize)]
struct ElevenLabsDialogueRequest<'a> {
    inputs: [ElevenLabsDialogueInput<'a>; 1],
    model_id: &'a str,
}

#[derive(Serialize)]
struct ElevenLabsDialogueInput<'a> {
    text: &'a str,
    voice_id: &'a str,
}

pub type Mp3Stream = Pin<Box<dyn Stream<Item = Result<Bytes, String>> + Send>>;

impl ElevenLabsClient {
    pub fn new(api_key: String, model_id: String, voice_id: String, output_format: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .tcp_nodelay(true)
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            http,
            api_key,
            endpoint: "https://api.elevenlabs.io".to_string(),
            model_id,
            voice_id,
            output_format,
        }
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    pub async fn synthesize_stream(&self, text: &str) -> Result<Mp3Stream, String> {
        let trimmed = text.trim();
        if !trimmed.chars().any(|c| c.is_alphabetic()) {
            return Ok(Box::pin(futures_util::stream::empty()));
        }

        let request = ElevenLabsDialogueRequest {
            inputs: [ElevenLabsDialogueInput {
                text: trimmed,
                voice_id: &self.voice_id,
            }],
            model_id: &self.model_id,
        };

        let url = format!(
            "{}/v1/text-to-dialogue/stream?output_format={}",
            self.endpoint.trim_end_matches('/'),
            self.output_format
        );

        let started = std::time::Instant::now();
        let response = self
            .http
            .post(&url)
            .header("xi-api-key", &self.api_key)
            .json(&request)
            .send()
            .await
            .map_err(|e| format!("ElevenLabs request send failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::error!(status = %status, error_body = %body, "ElevenLabs TTS API error");
            return Err(format!("ElevenLabs TTS failed ({status}): {body}"));
        }

        tracing::debug!(
            char_count = text.len(),
            latency_ms = started.elapsed().as_millis(),
            "ElevenLabs TTS stream connected"
        );

        let stream = response
            .bytes_stream()
            .map(|chunk| chunk.map_err(|e| e.to_string()));

        Ok(Box::pin(stream))
    }
}
