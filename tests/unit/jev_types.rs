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
