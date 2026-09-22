/**
* Device registration, authorization tokens, and capability discovery.
*/
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimJobsRequest {
    pub max_jobs: usize,
    pub supported_kinds: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubmitJobResultRequest {
    pub lease_generation: i64,
    pub outcome: String,
    pub result_reference: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailJobRequest {
    pub lease_generation: i64,
    pub error_code: String,
    pub error_details: Option<String>,
}
