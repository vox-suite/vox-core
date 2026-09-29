/**
 * AssemblyAI batch transcription client for complete voice utterances.
 */
use serde::Deserialize;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct AssemblyAiClient {
    http: reqwest::Client,
    api_key: String,
    endpoint: String,
    speech_model: String,
}

#[derive(Deserialize)]
struct UploadResponse {
    upload_url: String,
}

#[derive(Deserialize)]
struct TranscriptResponse {
    id: String,
    status: String,
    text: Option<String>,
    error: Option<String>,
}

impl AssemblyAiClient {
    pub fn new(api_key: String, speech_model: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            http,
            api_key,
            endpoint: "https://api.assemblyai.com".to_string(),
            speech_model,
        }
    }

    pub fn with_endpoint(mut self, endpoint: String) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Transcribes one complete utterance of 16-bit mono PCM at `sample_rate`
    /// Hz. Returns an empty string for silence/no speech, never an error for
    /// "nothing was said" -- only for genuine upload/API failures.
    pub async fn transcribe_pcm16(&self, pcm: &[i16], sample_rate: u32) -> Result<String, String> {
        if pcm.is_empty() {
            return Ok(String::new());
        }

        let wav = wrap_pcm16_as_wav(pcm, sample_rate);

        let started = std::time::Instant::now();
        let upload: UploadResponse = self
            .http
            .post(format!("{}/v2/upload", self.endpoint.trim_end_matches('/')))
            .header("authorization", &self.api_key)
            .body(wav)
            .send()
            .await
            .map_err(|e| format!("AssemblyAI upload request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("AssemblyAI upload rejected: {e}"))?
            .json()
            .await
            .map_err(|e| format!("AssemblyAI upload response invalid: {e}"))?;

        let job: TranscriptResponse = self
            .http
            .post(format!(
                "{}/v2/transcript",
                self.endpoint.trim_end_matches('/')
            ))
            .header("authorization", &self.api_key)
            .json(&serde_json::json!({
                "audio_url": upload.upload_url,
                "speech_model": self.speech_model,
            }))
            .send()
            .await
            .map_err(|e| format!("AssemblyAI transcript request failed: {e}"))?
            .error_for_status()
            .map_err(|e| format!("AssemblyAI transcript job rejected: {e}"))?
            .json()
            .await
            .map_err(|e| format!("AssemblyAI transcript response invalid: {e}"))?;

        let poll_url = format!(
            "{}/v2/transcript/{}",
            self.endpoint.trim_end_matches('/'),
            job.id
        );

        // Utterances here are a few seconds at most; 30 polls at 500ms
        // covers well past any realistic completion time before giving up.
        for _ in 0..30 {
            tokio::time::sleep(Duration::from_millis(500)).await;

            let poll: TranscriptResponse = self
                .http
                .get(&poll_url)
                .header("authorization", &self.api_key)
                .send()
                .await
                .map_err(|e| format!("AssemblyAI poll request failed: {e}"))?
                .error_for_status()
                .map_err(|e| format!("AssemblyAI poll rejected: {e}"))?
                .json()
                .await
                .map_err(|e| format!("AssemblyAI poll response invalid: {e}"))?;

            match poll.status.as_str() {
                "completed" => {
                    tracing::debug!(
                        sample_count = pcm.len(),
                        latency_ms = started.elapsed().as_millis(),
                        "AssemblyAI transcription completed"
                    );
                    return Ok(poll.text.unwrap_or_default());
                }
                "error" => {
                    return Err(format!(
                        "AssemblyAI transcription failed: {}",
                        poll.error.unwrap_or_default()
                    ));
                }
                _ => continue,
            }
        }

        Err("AssemblyAI transcription timed out".to_string())
    }
}

fn wrap_pcm16_as_wav(pcm: &[i16], sample_rate: u32) -> Vec<u8> {
    let data_len = pcm.len() * 2;
    let mut wav = Vec::with_capacity(44 + data_len);

    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&((36 + data_len) as u32).to_le_bytes());
    wav.extend_from_slice(b"WAVE");

    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM format
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&(sample_rate * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample

    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&(data_len as u32).to_le_bytes());
    for sample in pcm {
        wav.extend_from_slice(&sample.to_le_bytes());
    }

    wav
}
