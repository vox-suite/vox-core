/*!
* Core library module root exporting domains, services, and shared utilities.
*/
// Moved to the vox-connections crate (shared with vox-bridge and reusable
// elsewhere); re-exported here so existing `crate::connections::X` etc.
// call sites throughout vox-core don't need to change.
pub use vox_connections::{capability_grants, connections, integration_registry};

pub mod agent_registry;
pub mod agents;
pub mod application;
pub mod approvals;
pub mod audit;
pub mod bridge_client;
pub mod config;
pub mod conformance;
pub mod connected_apps;
pub mod consent;
pub mod conversations;
pub mod core_api_client;
pub mod db;
pub mod devices;
pub mod domain;
pub mod durable_tasks;
pub mod events;
pub mod execution;
pub mod execution_policy;
pub mod host_trust;
pub mod http;
pub mod identity;
pub mod identity_adapters;
pub mod ingestion;
pub mod jev;
pub mod jobs;
pub mod location_ingestion;
pub mod memory;
pub mod outbound;
pub mod preferences;
pub mod privacy;
pub mod providers;
pub mod realtime;
pub mod redis_keys;
pub mod reminders;
pub mod remote_extensions;
pub mod schedules;
pub mod skills;
pub mod sms_ingestion;
pub mod status;
pub mod storage;
pub mod summaries;
pub mod telemetry;
pub mod workers;
