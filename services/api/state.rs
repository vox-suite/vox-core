/**
* Shared application state container for API request handlers.
*/
use sqlx::PgPool;
use vox_core::{
    application::{
        collections::CollectionService, devices::DeviceService, records::RecordService,
        schemas::SchemaService, tasks::TaskService,
    },
    db::Db,
    http::AppState,
    realtime::DeviceHub,
    storage::{
        collections::CollectionRepository, devices::DeviceRepository, records::RecordRepository,
        schemas::SchemaRepository, tasks::TaskRepository,
    },
};

#[derive(Clone)]
pub struct ApiState {
    pub legacy: AppState,
    pub pool: PgPool,
    pub tasks: TaskService,
    pub collections: CollectionService,
    pub records: RecordService,
    pub schemas: SchemaService,
    pub devices: DeviceService,
    pub device_hub: DeviceHub,
}

impl ApiState {
    pub fn new(legacy: AppState, db: Db, device_hub: DeviceHub) -> Self {
        let pool = db.pool().clone();
        let coll_repo = CollectionRepository::new(pool.clone());
        let task_repo = TaskRepository::new(pool.clone());
        let rec_repo = RecordRepository::new(pool.clone());
        let schema_repo = SchemaRepository::new(pool.clone());
        let device_repo = DeviceRepository::new(pool.clone());

        let tasks = TaskService::new(task_repo, coll_repo.clone());
        let collections = CollectionService::new(coll_repo.clone());
        let records = RecordService::new(rec_repo, schema_repo.clone(), coll_repo);
        let schemas = SchemaService::new(schema_repo);
        let devices = DeviceService::new(device_repo);

        Self {
            legacy,
            pool,
            tasks,
            collections,
            records,
            schemas,
            devices,
            device_hub,
        }
    }
}
