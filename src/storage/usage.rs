use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Debug, Default)]
pub struct GeminiUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub reasoning_tokens: Option<i64>,
    pub cached_input_tokens: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct VoiceUsage {
    pub session_seconds: Option<f64>,
    pub input_audio_seconds: Option<f64>,
    pub output_audio_seconds: Option<f64>,
    pub tts_chars: Option<i64>,
}

#[derive(Clone)]
pub struct UsageRepository {
    pool: PgPool,
}

impl UsageRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn insert_gemini(
        &self,
        user: Uuid,
        source_key: &str,
        channel: &str,
        feature: &str,
        model: Option<&str>,
        usage: &GeminiUsage,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO usage_events (user_id, kind, source_key, channel, feature, model, input_tokens, output_tokens, reasoning_tokens, cached_input_tokens)
             VALUES ($1, 'gemini', $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (user_id, kind, source_key) DO NOTHING",
        )
        .bind(user)
        .bind(source_key)
        .bind(channel)
        .bind(feature)
        .bind(model)
        .bind(usage.input_tokens)
        .bind(usage.output_tokens)
        .bind(usage.reasoning_tokens)
        .bind(usage.cached_input_tokens)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }

    pub async fn insert_voice(
        &self,
        user: Uuid,
        source_key: &str,
        channel: &str,
        usage: &VoiceUsage,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            "INSERT INTO usage_events (user_id, kind, source_key, channel, feature, session_seconds, input_audio_seconds, output_audio_seconds, tts_chars)
             VALUES ($1, 'voice', $2, $3, 'voice_session', $4, $5, $6, $7)
             ON CONFLICT (user_id, kind, source_key) DO NOTHING",
        )
        .bind(user)
        .bind(source_key)
        .bind(channel)
        .bind(usage.session_seconds)
        .bind(usage.input_audio_seconds)
        .bind(usage.output_audio_seconds)
        .bind(usage.tts_chars)
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected() == 1)
    }
}
