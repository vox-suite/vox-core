use vox_core::agents::{
    event_planner::{PlannedAction, parse_planned_actions},
    summarizer::parse_summary,
};

#[test]
fn parses_a_versioned_outbound_call_action() {
    let actions = parse_planned_actions(
        r#"{"version":1,"actions":[{"kind":"outbound_call","reason":"Weather warning","opening_instruction":"Explain the warning"}]}"#,
    )
    .expect("valid action plan");

    assert_eq!(
        actions,
        vec![PlannedAction::OutboundCall {
            reason: "Weather warning".into(),
            opening_instruction: "Explain the warning".into(),
        }]
    );
}

#[test]
fn rejects_unknown_action_kinds() {
    let error = parse_planned_actions(
        r#"{"version":1,"actions":[{"kind":"delete_account","reason":"x","opening_instruction":"x"}]}"#,
    )
    .expect_err("unknown actions must not execute");

    assert_eq!(
        error.to_string(),
        "agent returned invalid structured output"
    );
}

#[test]
fn rejects_an_incomplete_summary() {
    let error = parse_summary(r#"{"recap":"Call recap"}"#)
        .expect_err("missing structured fields must fail");

    assert_eq!(
        error.to_string(),
        "agent returned invalid structured output"
    );
}
