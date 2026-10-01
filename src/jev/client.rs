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
    #[error("Jev API returned error status {0}")]
    Api(StatusCode),
    #[error("Database lookup for classification failed")]
    SchemaLookup,
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
            // Provider errors may echo prompts or credentials. Keep only status,
            // including in the error returned to callers and their logs.
            tracing::warn!(%status, "Jev API returned error");
            return Err(JevError::Api(status));
        }

        let response: SystemOneResponse = res.json().await?;
        tracing::debug!(
            duration_ms = start.elapsed().as_millis(),
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

#[cfg(test)]
mod content_redaction_tests {
    use super::*;
    use crate::jev::ToolRouter;
    use axum::{Json, Router, routing::post};
    use serde_json::json;
    use std::io::{self, Write};
    use std::sync::{Arc, Mutex};
    use tracing::instrument::WithSubscriber;

    #[derive(Clone)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl Write for CapturedLogs {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    async fn exercise(
        response: Router,
    ) -> (String, Result<(super::super::ToolDomain, f64), JevError>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, response).await.unwrap();
        });
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let writer = CapturedLogs(buffer.clone());
        let subscriber = tracing_subscriber::fmt()
            .without_time()
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let router = ToolRouter::new(JevClient::new(
            "test-only-key".into(),
            Some(format!("http://{address}/")),
        ));
        let result = router
            .classify("private-prompt-canary-9182")
            .with_subscriber(subscriber)
            .await;
        server.abort();
        let logs = String::from_utf8(buffer.lock().unwrap().clone()).unwrap();
        (logs, result)
    }

    #[tokio::test]
    async fn classifier_logs_route_metadata_without_conversation_content() {
        let response = Router::new().route(
            "/",
            post(|Json(request): Json<Value>| async move {
                assert_eq!(
                    request["state"]["user_prompt"],
                    "private-prompt-canary-9182"
                );
                Json(json!({
                    "model": "test-model",
                    "answers": {"decision": {
                        "type": "choice", "choice": "calendar", "confidence": 0.9,
                        "probabilities": {"calendar": 0.9}
                    }}
                }))
            }),
        );
        let (logs, result) = exercise(response).await;
        assert_eq!(result.unwrap(), (super::super::ToolDomain::Calendar, 0.9));
        assert!(logs.contains("domain=Calendar"), "{logs}");
        assert!(!logs.contains("private-prompt-canary-9182"), "{logs}");
    }

    #[tokio::test]
    async fn classifier_failure_does_not_retain_or_log_provider_body() {
        let response = Router::new().route(
            "/",
            post(|| async { (StatusCode::BAD_GATEWAY, "private-provider-body-canary-7319") }),
        );
        let (logs, result) = exercise(response).await;
        let error = result.unwrap_err();
        let diagnostics = format!("{logs}\n{error}\n{error:?}");
        assert!(diagnostics.contains("502"), "{diagnostics}");
        assert!(
            !diagnostics.contains("private-provider-body-canary-7319"),
            "{diagnostics}"
        );
    }
}
