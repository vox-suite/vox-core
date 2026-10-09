use sqlx::PgPool;
/**
* Shared application state container for API request handlers.
*/
use std::sync::Arc;
use vox_core::{
    agents::chart_suggester::SuggestingCharts,
    application::{
        collections::CollectionService, devices::DeviceService, 
        schemas::SchemaService, spans::SpanService,
    },
    consent::ConsentService,
    db::Db,
    http::AppState,
    memory::MemoryService,
    realtime::{DeviceHub, UserEventHub},
    storage::{
        pulse::PulseRepository, collections::CollectionRepository, devices::DeviceRepository,
         schemas::SchemaRepository, spans::SpanRepository,
    },
};

#[derive(Clone)]
pub struct ApiState {
    pub legacy: AppState,
    pub pool: PgPool,
    pub spans: SpanService,
    pub collections: CollectionService,
    pub schemas: SchemaService,
    pub devices: DeviceService,
    pub device_hub: DeviceHub,
    pub memory: MemoryService,
    pub user_events: UserEventHub,
    pub consent: ConsentService,
    pub tts: Option<std::sync::Arc<vox_core::tts::ElevenLabsClient>>,
    pub stt: Option<std::sync::Arc<vox_core::stt::AssemblyAiClient>>,
    pub charts: PulseRepository,
    pub chart_suggester: Arc<dyn SuggestingCharts>,
    pub spaces: vox_core::storage::spaces::SpaceRepository,
    pub space_runtime: Arc<vox_core::agents::space_runtime::SpaceRuntime>,
    pub bridge: Option<Arc<dyn vox_core::bridge_client::OutboundBridge>>,
    pub connections: vox_core::fresh_connections::FreshConnectionsService,
    pub timeline: vox_core::application::timeline::TimelineService,
    pub updates: vox_core::application::updates::UpdatesService,
    pub tool_export: vox_core::agents::tools::export::ToolExport,
}

impl ApiState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        legacy: AppState,
        db: Db,
        device_hub: DeviceHub,
        memory: MemoryService,
        user_events: UserEventHub,
        config: &vox_core::config::Config,
        tts: Option<std::sync::Arc<vox_core::tts::ElevenLabsClient>>,
        stt: Option<std::sync::Arc<vox_core::stt::AssemblyAiClient>>,
        chart_suggester: Arc<dyn SuggestingCharts>,
        _space_architect: Arc<dyn vox_core::agents::space_architect::SpaceArchitecting>,
        space_runtime: Arc<vox_core::agents::space_runtime::SpaceRuntime>,
    ) -> Self {
        let pool = db.pool().clone();
        let coll_repo = CollectionRepository::new(pool.clone());
        let span_repo = SpanRepository::new(pool.clone());
        let schema_repo = SchemaRepository::new(pool.clone());
        let device_repo = DeviceRepository::new(pool.clone());
        let chart_repo = PulseRepository::new(pool.clone());
        let space_repo = vox_core::storage::spaces::SpaceRepository::new(pool.clone());
        let timeline_repo = vox_core::storage::timeline::TimelineRepository::new(pool.clone());
        let updates_repo = vox_core::storage::updates::UpdatesRepository::new(pool.clone());

        let spans = SpanService::new(span_repo, user_events.clone());
        let collections = CollectionService::new(coll_repo.clone());
        let schemas = SchemaService::new(schema_repo);
        let devices = DeviceService::new(device_repo);
        let consent = ConsentService::new(db.clone());
        let timeline = vox_core::application::timeline::TimelineService::new(timeline_repo);
        let updates = vox_core::application::updates::UpdatesService::new(updates_repo);
        let connections = vox_core::fresh_connections::FreshConnectionsService::new(
            pool.clone(),
            config.credential_key.as_deref(),
            Some(user_events.clone()),
            config.google_client_id.clone(),
            config.google_client_secret.clone(),
            config.core_api_url.clone(),
        )
        .expect("FreshConnectionsService initialization failed");

        let tool_export = vox_core::agents::tools::export::ToolExport {
            db: db.clone(),
            memory: memory.clone(),
            connections: connections.clone(),
            user_events: user_events.clone(),
            device_hub: device_hub.clone(),
            google_maps_api_key: config.google_maps_api_key.clone(),
        };

        Self {
            legacy,
            pool,
            spans,
            collections,
            schemas,
            devices,
            device_hub,
            memory,
            user_events,
            consent,
            tts,
            stt,
            charts: chart_repo,
            chart_suggester,
            spaces: space_repo,
            space_runtime,
            bridge: None,
            connections,
            timeline,
            updates,
            tool_export,
        }
    }
}
