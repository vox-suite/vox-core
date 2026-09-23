/**
* Integration tests for configuration loading and environment overrides.
*/
use std::collections::HashMap;
use vox_core::config::Config;

fn values() -> HashMap<&'static str, &'static str> {
    HashMap::from([
        ("DATABASE_URL", "postgres://vox:test@db/vox"),
        ("VOX_AUTH_TOKEN", "test-service-token"),
        ("GEMINI_API_KEY", "gemini-test-key"),
        ("EXA_API_KEY", "exa-test-key"),
    ])
}

#[test]
fn loads_required_configuration_without_redis() {
    let values = values();
    let config = Config::from_values(|name| values.get(name).map(|value| value.to_string()))
        .expect("valid configuration");

    assert_eq!(config.bind_address, "0.0.0.0:3001");
    assert_eq!(config.database_url, "postgres://vox:test@db/vox");
    assert_eq!(config.redis_url, None);
    assert_eq!(config.service_token, "test-service-token");
    assert_eq!(config.gemini_api_key, "gemini-test-key");
    assert_eq!(config.exa_api_key, "exa-test-key");
    assert_eq!(config.gemini_model, "gemini-3.5-flash-lite");
    assert_eq!(config.jev_api_key, None);
    assert!(!config.jev_enabled);
    assert_eq!(config.jev_base_url, "https://api.typesafe.ai/v1/systemone");
    assert_eq!(config.tts_provider, "elevenlabs");
}

#[test]
fn loads_custom_tts_provider_when_set() {
    let mut values = values();
    values.insert("VOX_TTS_PROVIDER", "sarvam");

    let config = Config::from_values(|name| values.get(name).map(|value| value.to_string()))
        .expect("valid configuration");

    assert_eq!(config.tts_provider, "sarvam");
}

#[test]
fn loads_jev_configuration_when_set() {
    let mut values = values();
    values.insert("JEV_API_KEY", "test-jev-token");

    let config = Config::from_values(|name| values.get(name).map(|value| value.to_string()))
        .expect("valid configuration");

    assert_eq!(config.jev_api_key, Some("test-jev-token".to_string()));
    assert!(config.jev_enabled);
}

#[test]
fn rejects_a_missing_database_url() {
    let mut values = values();
    values.remove("DATABASE_URL");

    let error = Config::from_values(|name| values.get(name).map(|value| value.to_string()))
        .expect_err("missing database URL must fail");

    assert_eq!(error.to_string(), "DATABASE_URL is missing");
}
