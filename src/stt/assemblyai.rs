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
    pub fn new(api_key: String) -> Self {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            http,
            api_key,
            endpoint: "https://api.assemblyai.com".to_string(),
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

        let upload_ms = started.elapsed().as_millis() as u64;
        tracing::info!(upload_ms, sample_count = pcm.len(), "VOICE_STT_UPLOAD");
        let create_started = std::time::Instant::now();
        let body = serde_json::json!({ "audio_url": upload.upload_url });
        let response = self
            .http
            .post(format!(
                "{}/v2/transcript",
                self.endpoint.trim_end_matches('/')
            ))
            .header("authorization", &self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("AssemblyAI transcript request failed: {e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let detail = response.text().await.unwrap_or_default();
            return Err(format!(
                "AssemblyAI transcript job rejected: {status}: {}",
                detail.chars().take(500).collect::<String>()
            ));
        }
        let job: TranscriptResponse = response
            .json()
            .await
            .map_err(|e| format!("AssemblyAI transcript response invalid: {e}"))?;

        let create_ms = create_started.elapsed().as_millis() as u64;
        tracing::info!(create_ms, "VOICE_STT_JOB_CREATED");
        let polling_started = std::time::Instant::now();
        let poll_url = format!(
            "{}/v2/transcript/{}",
            self.endpoint.trim_end_matches('/'),
            job.id
        );

        // Utterances here are a few seconds at most; 30 polls at 500ms
        // covers well past any realistic completion time before giving up.
        for poll_index in 0..30 {
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
                    tracing::info!(
                        sample_count = pcm.len(),
                        upload_ms,
                        create_ms,
                        polls = poll_index + 1,
                        scheduled_poll_wait_ms = (poll_index + 1) * 500,
                        polling_ms = polling_started.elapsed().as_millis() as u64,
                        total_ms = started.elapsed().as_millis() as u64,
                        "VOICE_STT_COMPLETED"
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
