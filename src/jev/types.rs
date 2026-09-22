/**
 * Data contracts and wire formats for Jev service communication.
 */

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SystemOneRequest {
    pub state: Value,
    pub model: String,
    pub questions: HashMap<String, Question>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Question {
    Noul {
        instructions: Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        criteria: Option<NoulCriteria>,
    },
    Choice {
        instructions: Value,
        criteria: HashMap<String, Option<String>>,
    },
    Score {
        instructions: Value,
        criteria: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoulCriteria {
    #[serde(rename = "true", skip_serializing_if = "Option::is_none")]
    pub truthy: Option<String>,
    #[serde(rename = "false", skip_serializing_if = "Option::is_none")]
    pub falsy: Option<String>,
}

impl Question {
    pub fn noul(instructions: impl Into<Value>) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: None,
        }
    }

    pub fn noul_with_criteria(
        instructions: impl Into<Value>,
        true_meaning: impl Into<String>,
        false_meaning: impl Into<String>,
    ) -> Self {
        Self::Noul {
            instructions: instructions.into(),
            criteria: Some(NoulCriteria {
                truthy: Some(true_meaning.into()),
                falsy: Some(false_meaning.into()),
            }),
        }
    }

    pub fn choice<I, K, V>(instructions: impl Into<Value>, options: I) -> Self
    where
        I: IntoIterator<Item = (K, Option<V>)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut criteria = HashMap::new();
        for (k, v) in options {
            criteria.insert(k.into(), v.map(Into::into));
        }
        Self::Choice {
            instructions: instructions.into(),
            criteria,
        }
    }

    pub fn choice_simple(instructions: impl Into<Value>, options: &[&str]) -> Self {
        let criteria = options.iter().map(|opt| (opt.to_string(), None)).collect();
        Self::Choice {
            instructions: instructions.into(),
            criteria,
        }
    }

    pub fn score(instructions: impl Into<Value>, levels: &[&str]) -> Self {
        Self::Score {
            instructions: instructions.into(),
            criteria: levels.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct SystemOneResponse {
    pub model: String,
    pub answers: HashMap<String, Answer>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: HashMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        #[serde(default)]
        legend: HashMap<String, String>,
        #[serde(default)]
        probabilities: HashMap<String, f64>,
        confidence: f64,
    },
}

impl Answer {
    pub fn as_choice(&self) -> Option<(&str, f64, &HashMap<String, f64>)> {
        match self {
            Self::Choice {
                choice,
                confidence,
                probabilities,
            } => Some((choice.as_str(), *confidence, probabilities)),
            _ => None,
        }
    }

    pub fn as_noul(&self) -> Option<f64> {
        match self {
            Self::Noul { noul } => Some(*noul),
            _ => None,
        }
    }

    pub fn as_score(&self) -> Option<(f64, f64)> {
        match self {
            Self::Score {
                score, confidence, ..
            } => Some((*score, *confidence)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn serializes_noul_question() {
        let q = Question::noul_with_criteria("Is this urgent?", "Yes urgent", "Not urgent");
        let serialized = serde_json::to_value(&q).unwrap();
        assert_eq!(serialized["type"], "noul");
        assert_eq!(serialized["instructions"], "Is this urgent?");
        assert_eq!(serialized["criteria"]["true"], "Yes urgent");
        assert_eq!(serialized["criteria"]["false"], "Not urgent");
    }

    #[test]
    fn serializes_choice_question() {
        let q = Question::choice(
            "Which department?",
            [
                ("billing", Some("Payments and invoices")),
                ("support", None),
            ],
        );
        let serialized = serde_json::to_value(&q).unwrap();
        assert_eq!(serialized["type"], "choice");
        assert_eq!(serialized["criteria"]["billing"], "Payments and invoices");
        assert!(serialized["criteria"]["support"].is_null());
    }

    #[test]
    fn deserializes_system_one_response() {
        let json_data = json!({
            "model": "jev-latest",
            "answers": {
                "is_urgent": {
                    "type": "noul",
                    "noul": 0.92
                },
                "category": {
                    "type": "choice",
                    "choice": "billing",
                    "probabilities": { "billing": 0.85, "support": 0.15 },
                    "confidence": 0.82
                },
                "frustration": {
                    "type": "score",
                    "score": 1.6,
                    "legend": { "0": "Calm", "1": "Frustrated", "2": "Angry" },
                    "probabilities": { "0": 0.05, "1": 0.3, "2": 0.65 },
                    "confidence": 0.78
                }
            },
            "usage": { "input_tokens": 312, "output_tokens": 48 }
        });

        let resp: SystemOneResponse = serde_json::from_value(json_data).unwrap();
        assert_eq!(resp.model, "jev-latest");
        assert_eq!(resp.answers.get("is_urgent").unwrap().as_noul(), Some(0.92));

        let (choice, conf, probs) = resp.answers.get("category").unwrap().as_choice().unwrap();
        assert_eq!(choice, "billing");
        assert_eq!(conf, 0.82);
        assert_eq!(probs.get("billing"), Some(&0.85));

        let (score, score_conf) = resp.answers.get("frustration").unwrap().as_score().unwrap();
        assert_eq!(score, 1.6);
        assert_eq!(score_conf, 0.78);
    }
}
