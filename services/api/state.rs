/**
 * Shared application state container for API request handlers.
 */

use vox_core::{
    application::{
        collections::CollectionService,
        devices::DeviceService,
        records::RecordService,
        schemas::SchemaService,
        tasks::TaskService,
    },
    db::Db,
    http::AppState,
    storage::{
        collections::CollectionRepository,
        devices::DeviceRepository,
        records::RecordRepository,
        schemas::SchemaRepository,
        tasks::TaskRepository,
    },
};

#[derive(Clone)]
pub struct ApiState {
    pub legacy: AppState,
    pub tasks: TaskService,
    pub collections: CollectionService,
    pub records: RecordService,
    pub schemas: SchemaService,
    pub devices: DeviceService,
}

impl ApiState {
    pub fn new(legacy: AppState, db: Db) -> Self {
        let pool = db.pool().clone();
        let tasks = TaskService::new(TaskRepository::new(pool.clone()));
        let collections = CollectionService::new(CollectionRepository::new(pool.clone()));
        let records = RecordService::new(RecordRepository::new(pool.clone()));
        let schemas = SchemaService::new(SchemaRepository::new(pool.clone()));
        let devices = DeviceService::new(DeviceRepository::new(pool));

        Self {
            legacy,
            tasks,
            collections,
            records,
            schemas,
            devices,
        }
    }
}
