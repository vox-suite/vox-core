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
