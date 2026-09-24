use super::*;

#[test]
fn test_extract_name_from_text() {
    assert_eq!(
        extract_name_from_text("My name is Rahul."),
        Some("Rahul".into())
    );
    assert_eq!(
        extract_name_from_text("my name is rahul"),
        Some("rahul".into())
    );
    assert_eq!(
        extract_name_from_text("I'm Rahul Sharma"),
        Some("Rahul Sharma".into())
    );
    assert_eq!(
        extract_name_from_text("Call me John Doe"),
        Some("John Doe".into())
    );
    assert_eq!(
        extract_name_from_text("Hi, my name is Rahul"),
        Some("Rahul".into())
    );
    assert_eq!(
        extract_name_from_text("Hey, I'm Rahul"),
        Some("Rahul".into())
    );
    assert_eq!(extract_name_from_text("Rahul"), Some("Rahul".into()));
    assert_eq!(extract_name_from_text("Nope."), None);
    assert_eq!(extract_name_from_text("Hello there"), None);
}

#[test]
fn test_verification_state_serialization() {
    let state = VerificationState::AwaitingName {
        original_user_id: UserId(Uuid::new_v4()),
        original_text: "Question".into(),
        original_user_name: "Rahul".into(),
    };
    let serialized = serde_json::to_string(&state).unwrap();
    let deserialized: VerificationState = serde_json::from_str(&serialized).unwrap();
    match deserialized {
        VerificationState::AwaitingName {
            original_user_name, ..
        } => {
            assert_eq!(original_user_name, "Rahul");
        }
        _ => panic!("unexpected state"),
    }
}
