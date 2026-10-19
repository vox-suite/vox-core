use crate::storage::usage::{GeminiUsage, UsageRepository};
use rig::agent::CompletionCall;
use sqlx::PgPool;
use std::sync::OnceLock;
use uuid::Uuid;

static REPOSITORY: OnceLock<UsageRepository> = OnceLock::new();

tokio::task_local! {
    static USER: Uuid;
}

pub fn init(pool: PgPool) {
    let _ = REPOSITORY.set(UsageRepository::new(pool));
}

pub async fn scope<F: std::future::Future>(user: Uuid, future: F) -> F::Output {
    USER.scope(user, future).await
}

pub fn current_user() -> Option<Uuid> {
    USER.try_with(|user| *user).ok()
}

pub fn channel(raw: &str) -> &'static str {
    match raw.trim().to_ascii_lowercase().as_str() {
        "phone" | "call" | "twilio" => "telephony",
        "voice" => "voice",
        "whatsapp" => "whatsapp",
        _ => "chat",
    }
}

fn tokens(value: u64, known: bool) -> Option<i64> {
    known.then(|| i64::try_from(value).unwrap_or(i64::MAX))
}

pub fn record_call(
    user: Option<Uuid>,
    channel: &str,
    feature: &'static str,
    model: &str,
    call: &CompletionCall,
) {
    let Some(repository) = REPOSITORY.get().cloned() else {
        return;
    };
    let Some(user) = user.or_else(current_user) else {
        tracing::warn!(feature, "USAGE_UNATTRIBUTED");
        return;
    };
    let usage = &call.usage;
    let known = usage.total_tokens > 0 || usage.input_tokens > 0 || usage.output_tokens > 0;
    let row = GeminiUsage {
        input_tokens: tokens(usage.input_tokens, known),
        output_tokens: tokens(usage.output_tokens, known),
        reasoning_tokens: tokens(usage.reasoning_tokens, known),
        cached_input_tokens: tokens(usage.cached_input_tokens, known),
    };
    let channel = channel.to_owned();
    let model = model.to_owned();
    tokio::spawn(async move {
        if let Err(error) = repository
            .insert_gemini(
                user,
                &Uuid::new_v4().to_string(),
                &channel,
                feature,
                Some(&model),
                &row,
            )
            .await
        {
            tracing::warn!(feature, %error, "USAGE_RECORD_FAILED");
        }
    });
}

pub fn record_calls(
    user: Option<Uuid>,
    feature: &'static str,
    model: &str,
    calls: &[CompletionCall],
) {
    for call in calls {
        record_call(user, "background", feature, model, call);
    }
}
