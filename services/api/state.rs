/**
* Shared application state container for API request handlers.
*/
use std::sync::Arc;
use sqlx::PgPool;
use vox_core::{
    agents::chart_suggester::SuggestingCharts,
    application::{
        collections::CollectionService, devices::DeviceService, records::RecordService,
        schemas::SchemaService, spans::SpanService,
    },
    consent::ConsentService,
    db::Db,
    http::AppState,
    location_ingestion::LocationIngestionService,
    memory::MemoryService,
    realtime::{DeviceHub, UserEventHub},
    sms_ingestion::SmsIngestionService,
    storage::{
        charts::ChartRepository, collections::CollectionRepository, devices::DeviceRepository,
        records::RecordRepository, schemas::SchemaRepository, spans::SpanRepository,
    },
};

#[derive(Clone)]
pub struct ApiState {
    pub legacy: AppState,
    pub pool: PgPool,
    pub spans: SpanService,
    pub collections: CollectionService,
    pub records: RecordService,
    pub schemas: SchemaService,
    pub devices: DeviceService,
    pub device_hub: DeviceHub,
    pub memory: MemoryService,
    pub user_events: UserEventHub,
    pub sms_ingestion: SmsIngestionService,
    pub location_ingestion: LocationIngestionService,
    pub consent: ConsentService,
    pub tts: Option<std::sync::Arc<vox_core::tts::ElevenLabsClient>>,
    pub stt: Option<std::sync::Arc<vox_core::stt::AssemblyAiClient>>,
    pub charts: ChartRepository,
    pub chart_suggester: Arc<dyn SuggestingCharts>,
}

impl ApiState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        legacy: AppState,
        db: Db,
        device_hub: DeviceHub,
        memory: MemoryService,
        user_events: UserEventHub,
        google_maps_api_key: Option<String>,
        tts: Option<std::sync::Arc<vox_core::tts::ElevenLabsClient>>,
        stt: Option<std::sync::Arc<vox_core::stt::AssemblyAiClient>>,
        chart_suggester: Arc<dyn SuggestingCharts>,
    ) -> Self {
        let pool = db.pool().clone();
        let coll_repo = CollectionRepository::new(pool.clone());
        let span_repo = SpanRepository::new(pool.clone());
        let rec_repo = RecordRepository::new(pool.clone());
        let schema_repo = SchemaRepository::new(pool.clone());
        let device_repo = DeviceRepository::new(pool.clone());
        let chart_repo = ChartRepository::new(pool.clone());

        let spans = SpanService::new(span_repo, user_events.clone());
        let collections = CollectionService::new(coll_repo.clone());
        let records = RecordService::new(rec_repo, schema_repo.clone(), coll_repo);
        let schemas = SchemaService::new(schema_repo);
        let devices = DeviceService::new(device_repo);
        let sms_ingestion = SmsIngestionService::new(db.clone());
        let location_ingestion = LocationIngestionService::new(db.clone(), google_maps_api_key);
        let consent = ConsentService::new(db.clone());

        Self {
            legacy,
            pool,
            spans,
            collections,
            records,
            schemas,
            devices,
            device_hub,
            memory,
            user_events,
            sms_ingestion,
            location_ingestion,
            consent,
            tts,
            stt,
            charts: chart_repo,
            chart_suggester,
        }
    }
}
