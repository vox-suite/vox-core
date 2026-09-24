use super::{VOICE_CALL_PREAMBLE, spoken_response};

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
