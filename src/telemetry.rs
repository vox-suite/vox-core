/**
* Process logging plus optional agent traces exported to Langfuse over OTLP.
*/
use base64::Engine;
use opentelemetry::{KeyValue, trace::TracerProvider as _};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource,
    error::OTelSdkResult,
    trace::{SdkTracerProvider, SpanData, SpanExporter},
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tracing::{Level, Metadata};
use tracing_subscriber::{
    Layer,
    filter::{FilterExt, LevelFilter, filter_fn},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

/// Target of the root span Core opens around each conversation turn.
pub const TURN_TARGET: &str = "vox_core::agent_turn";

const DEFAULT_LANGFUSE_BASE_URL: &str = "https://cloud.langfuse.com";

static RECORD_CONTENT: AtomicBool = AtomicBool::new(false);

/// Whether traces may carry prompts, replies, and tool arguments/results.
/// Off unless `LANGFUSE_RECORD_CONTENT=true`: conversation text holds names,
/// addresses, and phone numbers.
pub fn record_content() -> bool {
    RECORD_CONTENT.load(Ordering::Relaxed)
}

/// Langfuse OTLP settings; tracing export stays off unless both keys are set.
pub struct LangfuseConfig {
    pub endpoint: String,
    pub authorization: String,
    pub record_content: bool,
    /// Keeps local and test traces out of production views.
    pub environment: Option<String>,
    /// Build or commit that produced the traces.
    pub release: Option<String>,
}

impl LangfuseConfig {
    pub fn from_env() -> Option<Self> {
        Self::from_values(|name| std::env::var(name).ok())
    }

    pub fn from_values(get: impl Fn(&str) -> Option<String>) -> Option<Self> {
        let value = |name: &str| {
            get(name)
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty())
        };
        let public_key = value("LANGFUSE_PUBLIC_KEY")?;
        let secret_key = value("LANGFUSE_SECRET_KEY")?;
        let base_url =
            value("LANGFUSE_BASE_URL").unwrap_or_else(|| DEFAULT_LANGFUSE_BASE_URL.to_string());
        let credentials =
            base64::engine::general_purpose::STANDARD.encode(format!("{public_key}:{secret_key}"));
        Some(Self {
            endpoint: format!(
                "{}/api/public/otel/v1/traces",
                base_url.trim_end_matches('/')
            ),
            authorization: format!("Basic {credentials}"),
            record_content: value("LANGFUSE_RECORD_CONTENT")
                .is_some_and(|flag| flag.eq_ignore_ascii_case("true") || flag == "1"),
            environment: value("LANGFUSE_TRACING_ENVIRONMENT"),
            release: value("LANGFUSE_RELEASE"),
        })
    }
}

/// Installs the global subscriber: INFO console logs as before, plus agent
/// spans sent to Langfuse when configured. Keep the returned provider alive for
/// the life of the process; dropping it stops the export.
pub fn init(service_name: &'static str) -> Option<SdkTracerProvider> {
    let config = LangfuseConfig::from_env();
    let (provider, export_error) = match config.as_ref().map(|c| exporter(c, service_name)) {
        Some(Ok(provider)) => (Some(provider), None),
        Some(Err(err)) => (None, Some(err)),
        None => (None, None),
    };
    let traces = provider.as_ref().map(|provider| {
        tracing_opentelemetry::layer()
            .with_tracer(provider.tracer("vox-core"))
            .with_filter(filter_fn(is_agent_span))
    });
    tracing_subscriber::registry()
        // Agent spans stay out of console logs: with content tracing on they
        // carry prompts and replies, which belong only in Langfuse.
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(LevelFilter::INFO.and(filter_fn(|metadata| !is_agent_span(metadata)))),
        )
        .with(traces)
        .init();

    if let Some(err) = export_error {
        tracing::warn!(%err, "Langfuse tracing disabled");
    }
    if let (Some(config), Some(_)) = (&config, &provider) {
        RECORD_CONTENT.store(config.record_content, Ordering::Relaxed);
        tracing::info!(
            endpoint = %config.endpoint,
            environment = config.environment.as_deref().unwrap_or("default"),
            record_content = config.record_content,
            "Agent traces are exported to Langfuse"
        );
    }
    provider
}

/// Only turn spans and rig's agent/chat/tool spans leave the process. Log
/// events are never exported: Core's tool logs include phone numbers and
/// queries, while rig's spans carry content only when `record_content` is on.
pub fn is_agent_span(metadata: &Metadata<'_>) -> bool {
    metadata.is_span()
        && *metadata.level() <= Level::INFO
        && (metadata.target() == TURN_TARGET || metadata.target().starts_with("rig"))
}

fn exporter(
    config: &LangfuseConfig,
    service_name: &'static str,
) -> Result<SdkTracerProvider, opentelemetry_otlp::ExporterBuildError> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(opentelemetry_otlp::Protocol::HttpBinary)
        .with_endpoint(&config.endpoint)
        .with_headers(HashMap::from([
            ("Authorization".to_string(), config.authorization.clone()),
            ("x-langfuse-ingestion-version".to_string(), "4".to_string()),
        ]))
        .build()?;
    let mut resource = Resource::builder().with_service_name(service_name);
    if let Some(environment) = &config.environment {
        resource = resource.with_attribute(KeyValue::new(
            "deployment.environment.name",
            environment.clone(),
        ));
    }
    if let Some(release) = &config.release {
        resource = resource.with_attribute(KeyValue::new("service.version", release.clone()));
    }
    Ok(SdkTracerProvider::builder()
        .with_resource(resource.build())
        .with_batch_exporter(NamedAgentSpans(exporter))
        .build())
}

/// Renames agent root spans after their agent, so each agent gets its own
/// trace name in Langfuse. tracing fixes a span's name when it is created, but
/// Core picks the conversation agent mid-turn and rig names every background
/// agent run `invoke_agent`.
#[derive(Debug)]
struct NamedAgentSpans<E>(E);

impl<E: SpanExporter> SpanExporter for NamedAgentSpans<E> {
    fn export(
        &self,
        mut batch: Vec<SpanData>,
    ) -> impl std::future::Future<Output = OTelSdkResult> + Send {
        batch.iter_mut().for_each(name_agent_span);
        self.0.export(batch)
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.0.force_flush()
    }

    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

fn name_agent_span(span: &mut SpanData) {
    // `conversation_turn` is the turn span in `agents::conversation`.
    if !matches!(span.name.as_ref(), "invoke_agent" | "conversation_turn") {
        return;
    }
    if let Some(agent) = span
        .attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == "gen_ai.agent.name")
    {
        span.name = agent.value.as_str().into_owned().into();
    }
}
