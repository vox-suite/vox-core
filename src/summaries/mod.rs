/**
* Conversation summary generation and storage.
*/
pub mod handler;

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredSummary {
    pub recap: String,
    pub profile_updates: BTreeMap<String, String>,
    pub commitments: Vec<String>,
    pub decisions: Vec<String>,
}
