/**
* HTTP client communicating with the Jev classification service.
*/
use super::types::{Answer, Question, SystemOneRequest, SystemOneResponse};
use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::{collections::HashMap, time::Duration};

pub const DEFAULT_JEV_URL: &str = "https://api.typesafe.ai/v1/systemone";
pub const DEFAULT_JEV_MODEL: &str = "jev-latest";

#[derive(Debug, thiserror::Error)]
pub enum JevError {
    #[error("http request to Jev failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Jev API returned error status {0}: {1}")]
    Api(StatusCode, String),
    #[error("missing answer for question '{0}'")]
    MissingAnswer(String),
    #[error("unexpected answer type for question '{0}'")]
    UnexpectedType(String),
    #[error("Jev is not configured or disabled")]
    NotConfigured,
}

#[derive(Clone)]
pub struct JevClient {
    http: Client,
    endpoint: String,
    api_key: String,
    model: String,
}

impl JevClient {
    pub fn new(api_key: String, endpoint: Option<String>) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(5))
            .tcp_nodelay(true)
            .build()
            .unwrap_or_default();
        Self {
            http,
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_JEV_URL.to_string()),
            api_key,
            model: DEFAULT_JEV_MODEL.to_string(),
        }
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub async fn evaluate(
        &self,
        state: Value,
        questions: HashMap<String, Question>,
    ) -> Result<SystemOneResponse, JevError> {
        let request = SystemOneRequest {
            state,
            model: self.model.clone(),
            questions,
        };

        let start = std::time::Instant::now();
        let res = self
            .http
            .post(&self.endpoint)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await?;

        let status = res.status();
        if !status.is_success() {
            let body = res.text().await.unwrap_or_default();
            tracing::warn!(%status, %body, "Jev API returned error");
            return Err(JevError::Api(status, body));
        }

        let response: SystemOneResponse = res.json().await?;
        tracing::debug!(
            duration_ms = start.elapsed().as_millis(),
            model = %response.model,
            questions_count = response.answers.len(),
            "Jev System 1 evaluation completed"
        );
        Ok(response)
    }

    pub async fn choice(
        &self,
        state: Value,
        question_instructions: &str,
        options: &[(&str, Option<&str>)],
    ) -> Result<(String, f64, HashMap<String, f64>), JevError> {
        let mut questions = HashMap::new();
        questions.insert(
            "decision".to_string(),
            Question::choice(question_instructions, options.iter().cloned()),
        );
        let resp = self.evaluate(state, questions).await?;
        let answer = resp
            .answers
            .get("decision")
            .ok_or_else(|| JevError::MissingAnswer("decision".into()))?;

        match answer {
            Answer::Choice {
                choice,
                confidence,
                probabilities,
            } => Ok((choice.clone(), *confidence, probabilities.clone())),
            _ => Err(JevError::UnexpectedType("decision".into())),
        }
    }

    pub async fn noul(&self, state: Value, question_instructions: &str) -> Result<f64, JevError> {
        let mut questions = HashMap::new();
        questions.insert("check".to_string(), Question::noul(question_instructions));
        let resp = self.evaluate(state, questions).await?;
        let answer = resp
            .answers
            .get("check")
            .ok_or_else(|| JevError::MissingAnswer("check".into()))?;

        answer
            .as_noul()
            .ok_or_else(|| JevError::UnexpectedType("check".into()))
    }
}
