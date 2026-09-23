/**
* Event triaging using fast LLM classification.
*/
use super::{JevError, client::JevClient, types::Question};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EventTriageAction {
    Ignore,
    StoreRecord,
    PlanAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EventTriageResult {
    pub action: EventTriageAction,
    pub confidence: f64,
    pub is_critical_alert: bool,
    pub alert_probability: f64,
}

#[derive(Clone)]
pub struct EventTriager {
    jev: JevClient,
}

impl EventTriager {
    pub fn new(jev: JevClient) -> Self {
        Self { jev }
    }

    pub async fn triage(
        &self,
        event_type: &str,
        payload: &Value,
    ) -> Result<EventTriageResult, JevError> {
        let state = json!({
            "event_type": event_type,
            "payload": payload,
        });

        let mut questions = HashMap::new();

        questions.insert(
            "action".to_string(),
            Question::choice(
                "What immediate handling does this event require?",
                vec![
                    ("ignore", Some("Routine telemetry, duplicate, ping, heartbeat, or benign event requiring no user action")),
                    ("store_record", Some("Passive personal metric, vitals, location breadcrumb, log, or telemetry to validate and record")),
                    ("plan_action", Some("Important change or event requiring notification, outbound call, or reactive task planning")),
                ],
            ),
        );

        questions.insert(
            "critical_alert".to_string(),
            Question::noul_with_criteria(
                "Does this event indicate an immediate emergency, crash, urgent medical distress, or severe hazard?",
                "Immediate real-world danger or distress requiring urgent intervention",
                "Normal or non-critical state",
            ),
        );

        let response = self.jev.evaluate(state, questions).await?;

        let (action_str, confidence, _) = response
            .answers
            .get("action")
            .and_then(|a| a.as_choice())
            .ok_or_else(|| JevError::MissingAnswer("action".into()))?;

        let alert_prob = response
            .answers
            .get("critical_alert")
            .and_then(|a| a.as_noul())
            .unwrap_or(0.0);

        let action = match action_str {
            "ignore" => EventTriageAction::Ignore,
            "store_record" => EventTriageAction::StoreRecord,
            _ => EventTriageAction::PlanAction,
        };

        Ok(EventTriageResult {
            action,
            confidence,
            is_critical_alert: alert_prob >= 0.80,
            alert_probability: alert_prob,
        })
    }
}
