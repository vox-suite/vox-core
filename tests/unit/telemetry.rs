use super::*;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::trace::InMemorySpanExporter;

fn config(values: &[(&str, &str)]) -> Option<LangfuseConfig> {
    let values: HashMap<&str, &str> = values.iter().copied().collect();
    LangfuseConfig::from_values(|name| values.get(name).map(|value| value.to_string()))
}

const KEYS: [(&str, &str); 2] = [
    ("LANGFUSE_PUBLIC_KEY", "pk-lf-1"),
    ("LANGFUSE_SECRET_KEY", "sk-lf-2"),
];

#[test]
fn stays_off_without_both_keys() {
    assert!(config(&[]).is_none());
    assert!(config(&KEYS[..1]).is_none());
    assert!(config(&[KEYS[0], ("LANGFUSE_SECRET_KEY", "  ")]).is_none());
}

#[test]
fn defaults_to_langfuse_cloud_without_content() {
    let config = config(&KEYS).expect("Langfuse configured");

    assert_eq!(
        config.endpoint,
        "https://cloud.langfuse.com/api/public/otel/v1/traces"
    );
    assert_eq!(config.authorization, "Basic cGstbGYtMTpzay1sZi0y");
    assert!(!config.record_content);
    assert_eq!(config.environment, None);
    assert_eq!(config.release, None);
}

#[test]
fn honours_region_and_content_opt_in() {
    let config = config(&[
        KEYS[0],
        KEYS[1],
        ("LANGFUSE_BASE_URL", "https://us.cloud.langfuse.com/"),
        ("LANGFUSE_RECORD_CONTENT", "TRUE"),
        ("LANGFUSE_TRACING_ENVIRONMENT", "production"),
        ("LANGFUSE_RELEASE", "2ad9329"),
    ])
    .expect("Langfuse configured");

    assert_eq!(
        config.endpoint,
        "https://us.cloud.langfuse.com/api/public/otel/v1/traces"
    );
    assert!(config.record_content);
    assert_eq!(config.environment.as_deref(), Some("production"));
    assert_eq!(config.release.as_deref(), Some("2ad9329"));
}

#[test]
fn exports_only_agent_spans_and_never_log_events() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let traces = tracing_opentelemetry::layer()
        .with_tracer(provider.tracer("test"))
        .with_filter(filter_fn(is_agent_span));

    tracing::subscriber::with_default(tracing_subscriber::registry().with(traces), || {
        tracing::info_span!(target: TURN_TARGET, "conversation_turn").in_scope(|| {
            tracing::info_span!(target: "rig::agent_chat", "chat").in_scope(|| {
                tracing::info!(phone_number = "+910000000000", "Tool called");
            });
            tracing::info_span!(target: "vox_core::http", "request").in_scope(|| {});
            tracing::debug_span!(target: "rig::completions", "verbose").in_scope(|| {});
        });
    });

    let spans = exporter.get_finished_spans().expect("finished spans");
    let names: Vec<&str> = spans.iter().map(|span| span.name.as_ref()).collect();
    assert_eq!(names, ["chat", "conversation_turn"]);
    let turn = &spans[1];
    assert_eq!(turn.parent_span_id, SpanId::INVALID);
    assert_eq!(spans[0].parent_span_id, turn.span_context.span_id());
    assert!(spans.iter().all(|span| span.events.is_empty()));
}

#[test]
fn names_agent_spans_after_their_agent() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let traces = tracing_opentelemetry::layer().with_tracer(provider.tracer("test"));

    tracing::subscriber::with_default(tracing_subscriber::registry().with(traces), || {
        tracing::info_span!("invoke_agent", gen_ai.agent.name = "summarizer-agent").in_scope(
            || {
                tracing::info_span!("execute_tool", gen_ai.tool.name = "web_search")
                    .in_scope(|| {});
            },
        );
        // The conversation agent is only known after the turn span started.
        let turn = tracing::info_span!(
            "conversation_turn",
            gen_ai.agent.name = tracing::field::Empty
        );
        turn.in_scope(|| {
            turn.record("gen_ai.agent.name", "shopping-agent");
            tracing::info_span!("chat", gen_ai.agent.name = "shopping-agent").in_scope(|| {});
        });
    });

    let mut spans = exporter.get_finished_spans().expect("finished spans");
    spans.iter_mut().for_each(name_agent_span);
    let names: Vec<&str> = spans.iter().map(|span| span.name.as_ref()).collect();
    assert_eq!(
        names,
        ["execute_tool", "summarizer-agent", "chat", "shopping-agent"]
    );
}
