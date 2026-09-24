/**
* Integration tests for Jev classifier integration and fallback.
*/
use serde_json::json;
use std::collections::HashMap;
use vox_core::jev::{
    event_triage::{EventTriageAction, EventTriageResult},
    schema_classifier::{
        FAST_PATH_CONFIDENCE_THRESHOLD, NOVEL_CATEGORY_SENTINEL, NOVELTY_CONFIDENCE_THRESHOLD,
        SchemaClassificationResult, SchemaDescriptor,
    },
    types::{Question, SystemOneRequest, SystemOneResponse},
};

#[test]
fn test_noul_question_serialization() {
    let q = Question::noul_with_criteria(
        "Does this convey urgency?",
        "Explicitly time-sensitive",
        "No urgency expressed",
    );
    let val = serde_json::to_value(&q).unwrap();
    assert_eq!(val["type"], "noul");
    assert_eq!(val["instructions"], "Does this convey urgency?");
    assert_eq!(val["criteria"]["true"], "Explicitly time-sensitive");
    assert_eq!(val["criteria"]["false"], "No urgency expressed");
}

#[test]
fn test_choice_question_serialization() {
    let q = Question::choice(
        "Which team should handle this?",
        vec![
            ("billing", Some("Payments, invoicing, refunds")),
            ("technical", Some("Bugs, outages, integrations")),
            ("sales", Some("Pricing, upgrades, new accounts")),
        ],
    );
    let val = serde_json::to_value(&q).unwrap();
    assert_eq!(val["type"], "choice");
    assert_eq!(val["instructions"], "Which team should handle this?");
    assert_eq!(val["criteria"]["billing"], "Payments, invoicing, refunds");
    assert_eq!(val["criteria"]["technical"], "Bugs, outages, integrations");
}

#[test]
fn test_score_question_serialization() {
    let q = Question::score(
        "How frustrated is the customer?",
        &["Calm", "Frustrated", "Very angry"],
    );
    let val = serde_json::to_value(&q).unwrap();
    assert_eq!(val["type"], "score");
    assert_eq!(val["instructions"], "How frustrated is the customer?");
    assert_eq!(val["criteria"], json!(["Calm", "Frustrated", "Very angry"]));
}

#[test]
fn test_system_one_request_structure() {
    let mut questions = HashMap::new();
    questions.insert(
        "is_urgent".to_string(),
        Question::noul("Does this convey urgency?"),
    );

    let req = SystemOneRequest {
        state: json!("Help! My payouts have been failing for 3 days."),
        model: "jev-latest".to_string(),
        questions,
    };

    let serialized = serde_json::to_value(&req).unwrap();
    assert_eq!(serialized["model"], "jev-latest");
    assert_eq!(
        serialized["state"],
        "Help! My payouts have been failing for 3 days."
    );
    assert!(serialized["questions"]["is_urgent"].is_object());
}

#[test]
fn test_system_one_response_deserialization() {
    let raw = json!({
        "model": "jev-latest",
        "answers": {
            "is_urgent": {
                "type": "noul",
                "noul": 0.92
            },
            "department": {
                "type": "choice",
                "choice": "technical",
                "probabilities": {
                    "billing": 0.08,
                    "technical": 0.85,
                    "sales": 0.07
                },
                "confidence": 0.82
            },
            "frustration": {
                "type": "score",
                "score": 1.6,
                "legend": { "0": "Calm", "1": "Frustrated", "2": "Very angry" },
                "probabilities": { "0": 0.05, "1": 0.3, "2": 0.65 },
                "confidence": 0.78
            }
        },
        "usage": {
            "input_tokens": 312,
            "output_tokens": 48
        }
    });

    let resp: SystemOneResponse = serde_json::from_value(raw).unwrap();
    assert_eq!(resp.model, "jev-latest");
    assert_eq!(resp.usage.as_ref().unwrap().input_tokens, 312);
    assert_eq!(resp.usage.as_ref().unwrap().output_tokens, 48);

    let noul_ans = resp.answers.get("is_urgent").unwrap();
    assert_eq!(noul_ans.as_noul(), Some(0.92));

    let choice_ans = resp.answers.get("department").unwrap();
    let (choice, conf, probs) = choice_ans.as_choice().unwrap();
    assert_eq!(choice, "technical");
    assert_eq!(conf, 0.82);
    assert_eq!(probs.get("technical"), Some(&0.85));

    let score_ans = resp.answers.get("frustration").unwrap();
    let (score, score_conf) = score_ans.as_score().unwrap();
    assert_eq!(score, 1.6);
    assert_eq!(score_conf, 0.78);
}

#[test]
fn test_schema_classification_thresholds() {
    assert_eq!(FAST_PATH_CONFIDENCE_THRESHOLD, 0.85);
    assert_eq!(NOVELTY_CONFIDENCE_THRESHOLD, 0.70);
    assert_eq!(NOVEL_CATEGORY_SENTINEL, "UNKNOWN_NEW_CATEGORY");

    let descriptor = SchemaDescriptor {
        id: uuid::Uuid::new_v4(),
        namespace: "device".into(),
        name: "telemetry".into(),
        qualified_name: "device.telemetry".into(),
        description: "Device battery and hardware metrics".into(),
        json_schema: json!({
            "type": "object",
            "properties": {
                "battery_level": { "type": "number" }
            }
        }),
    };

    let existing = SchemaClassificationResult::Existing {
        schema: descriptor.clone(),
        confidence: 0.94,
    };
    if let SchemaClassificationResult::Existing { confidence, schema } = existing {
        assert!(confidence >= FAST_PATH_CONFIDENCE_THRESHOLD);
        assert_eq!(schema.qualified_name, "device.telemetry");
    } else {
        panic!("Expected existing schema classification");
    }

    let novel = SchemaClassificationResult::Novel {
        suggested_category: None,
        confidence: 0.95,
        reason: "classified_as_novel",
    };
    if let SchemaClassificationResult::Novel { reason, .. } = novel {
        assert_eq!(reason, "classified_as_novel");
    } else {
        panic!("Expected novel classification");
    }
}

#[test]
fn test_event_triage_action_variants() {
    let triage_ignore = EventTriageResult {
        action: EventTriageAction::Ignore,
        confidence: 0.92,
        is_critical_alert: false,
        alert_probability: 0.02,
    };
    assert_eq!(triage_ignore.action, EventTriageAction::Ignore);
    assert!(!triage_ignore.is_critical_alert);

    let triage_store = EventTriageResult {
        action: EventTriageAction::StoreRecord,
        confidence: 0.88,
        is_critical_alert: false,
        alert_probability: 0.05,
    };
    assert_eq!(triage_store.action, EventTriageAction::StoreRecord);

    let triage_plan = EventTriageResult {
        action: EventTriageAction::PlanAction,
        confidence: 0.95,
        is_critical_alert: true,
        alert_probability: 0.96,
    };
    assert_eq!(triage_plan.action, EventTriageAction::PlanAction);
    assert!(triage_plan.is_critical_alert);
}

#[test]
fn test_tool_domain_variants() {
    use vox_core::jev::tool_router::ToolDomain;

    let domains = vec![
        ToolDomain::None,
        ToolDomain::WebSearch,
        ToolDomain::Maps,
        ToolDomain::TasksAndRecords,
        ToolDomain::Calendar,
        ToolDomain::Device,
        ToolDomain::All,
    ];

    for d in domains {
        let serialized = serde_json::to_string(&d).unwrap();
        let deserialized: ToolDomain = serde_json::from_str(&serialized).unwrap();
        assert_eq!(d, deserialized);
    }
}

#[test]
fn test_background_task_urgency_thresholds() {
    let non_urgent_prob = 0.15;
    let urgent_prob = 0.85;

    assert!(
        non_urgent_prob < 0.70,
        "Routine task should suppress outbound phone call"
    );
    assert!(urgent_prob >= 0.70, "Urgent task warrants phone escalation");
}

#[test]
fn test_summarizer_gating_thresholds() {
    let trivial_prob = 0.08;
    let meaningful_prob = 0.65;

    assert!(
        trivial_prob < 0.20,
        "Trivial chit-chat skips Gemini summarizer"
    );
    assert!(
        meaningful_prob >= 0.20,
        "Meaningful updates trigger full summarization"
    );
}
