use super::*;

#[test]
fn prompt_requires_natural_speech_only() {
    for requirement in [
        "speaking live",
        "Never use Markdown",
        "URLs",
        "one to three short sentences",
    ] {
        assert!(VOICE_CALL_PREAMBLE.contains(requirement));
        assert!(ELEVENLABS_VOICE_CALL_PREAMBLE.contains(requirement));
    }
}

#[test]
fn general_preamble_recognizes_multi_device_assistant() {
    assert!(GENERAL_PREAMBLE.contains("desktop, mobile, voice, and messaging"));
    assert!(!GENERAL_PREAMBLE.contains("phone call"));
}

#[test]
fn detects_voice_channels_correctly() {
    assert!(is_voice_channel("phone"));
    assert!(is_voice_channel("voice"));
    assert!(is_voice_channel("call"));
    assert!(is_voice_channel("Phone"));
    assert!(is_voice_channel("twilio"));
    assert!(is_voice_channel("Twilio"));
    assert!(!is_voice_channel("whatsapp"));
    assert!(!is_voice_channel("desktop"));
    assert!(!is_voice_channel("mobile"));
    assert!(!is_voice_channel("web"));
}

#[test]
fn selects_correct_preamble_by_channel() {
    assert_eq!(preamble_for_channel("phone"), VOICE_CALL_PREAMBLE);
    assert_eq!(preamble_for_channel("twilio"), VOICE_CALL_PREAMBLE);
    assert_eq!(preamble_for_channel("whatsapp"), WHATSAPP_PREAMBLE);
    assert_eq!(preamble_for_channel("desktop"), GENERAL_PREAMBLE);
    assert_eq!(preamble_for_channel("mobile"), GENERAL_PREAMBLE);
}

#[test]
fn selects_elevenlabs_preamble_for_voice() {
    assert_eq!(
        preamble_for_channel_and_tts("phone", Some("elevenlabs")),
        ELEVENLABS_VOICE_CALL_PREAMBLE
    );
    assert_eq!(
        preamble_for_channel_and_tts("twilio", Some("elevenlabs")),
        ELEVENLABS_VOICE_CALL_PREAMBLE
    );
    assert_eq!(
        preamble_for_channel_and_tts("phone", None),
        ELEVENLABS_VOICE_CALL_PREAMBLE
    );
    assert_eq!(
        preamble_for_channel_and_tts("phone", Some("sarvam")),
        VOICE_CALL_PREAMBLE
    );
}

#[test]
fn elevenlabs_preamble_contains_v3_best_practices() {
    for requirement in [
        "Audio Tags",
        "Punctuation and Pauses",
        "Emphasis and Pronunciation",
        "ellipses (...)",
        "International Phonetic Alphabet",
    ] {
        assert!(ELEVENLABS_VOICE_CALL_PREAMBLE.contains(requirement));
    }
}
