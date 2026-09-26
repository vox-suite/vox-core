use super::{VOICE_CALL_PREAMBLE, mentions_shopping, spoken_response};

#[test]
fn prompt_requires_natural_speech_only() {
    for requirement in [
        "speaking live",
        "Never use Markdown",
        "URLs",
        "one to three short sentences",
    ] {
        assert!(VOICE_CALL_PREAMBLE.contains(requirement));
    }
}

#[test]
fn normalizes_written_formatting_for_tts() {
    assert_eq!(
        spoken_response(
            "## Update\n1. **Traffic** is heavy.\n2. https://example.com See [the map](https://example.com)."
        ),
        "Update Traffic is heavy. See the map."
    );
}

#[test]
fn preserves_natural_conversational_speech() {
    assert_eq!(
        spoken_response("It looks busy near your office, so I'd leave ten minutes early."),
        "It looks busy near your office, so I'd leave ten minutes early."
    );
}

#[test]
fn preserves_audio_tags_and_pauses_for_elevenlabs() {
    assert_eq!(
        spoken_response(
            "[thoughtful] Let me check your calendar... [happy] You are free tomorrow!"
        ),
        "[thoughtful] Let me check your calendar... [happy] You are free tomorrow!"
    );
}

#[test]
fn normalizes_currency_and_abbreviations_for_speech() {
    assert_eq!(
        spoken_response("Apple stock is currently at $235.40. India vs. West Indies on Oct. 2."),
        "Apple stock is currently at 235 dollars and 40 cents. India versus West Indies on October 2."
    );
}

#[test]
fn splits_spoken_sentences_without_breaking_decimals_or_producing_letterless_chunks() {
    use super::split_spoken_sentences;

    let sentences = split_spoken_sentences(
        "Apple is at $235.40 right now. India plays on Oct. 2, 2025. That is great!",
    );
    assert_eq!(
        sentences,
        vec![
            "Apple is at $235.40 right now.",
            "India plays on Oct. 2, 2025.",
            "That is great!"
        ]
    );
    for s in sentences {
        assert!(s.chars().any(|c| c.is_alphabetic()));
    }
}

#[test]
fn detects_purchase_requests_for_shopping_route() {
    assert!(mentions_shopping("I want to buy an iPhone 16"));
    assert!(mentions_shopping("Order me AA batteries from Amazon"));
    assert!(!mentions_shopping("What's the weather in Chennai?"));
    assert!(!mentions_shopping("Remind me to call mom"));
}

#[tokio::test]
async fn traced_reply_records_what_was_said_and_marks_failures() {
    use super::{AgentError, ConversationPrompt, traced_reply, turn_span};
    use crate::identity::{ResourceOwner, UserContextId, UserId};
    use futures_util::StreamExt;
    use opentelemetry::trace::{Status, TracerProvider as _};
    use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
    use tracing_subscriber::layer::SubscriberExt;
    use uuid::Uuid;

    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let _subscriber = tracing::subscriber::set_default(
        tracing_subscriber::registry()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test"))),
    );
    let user_id = UserId(Uuid::new_v4());
    let prompt = ConversationPrompt {
        user_id,
        owner: ResourceOwner {
            user_context_id: UserContextId(Uuid::new_v4()),
            user_id,
        },
        channel: "voice".into(),
        user_context: String::new(),
        recent_messages: Vec::new(),
        user_text: "Order me a charger".into(),
        initiation_context: None,
        needs_onboarding: false,
        tts_provider: None,
        filler: None,
        conversation_id: Some(Uuid::nil()),
    };

    let chunks = futures_util::stream::iter([
        Ok("Sure, ".to_string()),
        Ok("ordering it.".to_string()),
        Err(AgentError::Provider),
    ]);
    let reply: Vec<_> = traced_reply(chunks, turn_span(&prompt), true)
        .collect()
        .await;
    assert_eq!(reply.len(), 3);

    let spans = exporter.get_finished_spans().expect("finished spans");
    let turn = spans
        .iter()
        .find(|span| span.name == "conversation_turn")
        .expect("turn span");
    let attribute = |key: &str| {
        turn.attributes
            .iter()
            .find(|kv| kv.key.as_str() == key)
            .map(|kv| kv.value.as_str().to_string())
    };
    assert_eq!(
        attribute("langfuse.observation.output").as_deref(),
        Some("Sure, ordering it.")
    );
    assert_eq!(
        attribute("session.id").as_deref(),
        Some("00000000-0000-0000-0000-000000000000")
    );
    assert_eq!(
        attribute("langfuse.observation.type").as_deref(),
        Some("agent")
    );
    assert_eq!(
        attribute("langfuse.trace.tags").as_deref(),
        Some(r#"["voice"]"#)
    );
    assert!(matches!(turn.status, Status::Error { .. }));
}
