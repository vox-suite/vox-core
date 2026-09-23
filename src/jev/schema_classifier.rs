/**
* Schema and entity classification logic for structured ingestion.
*/
use super::{JevError, client::JevClient};
use crate::db::Db;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::Row;
use uuid::Uuid;

pub const NOVEL_CATEGORY_SENTINEL: &str = "UNKNOWN_NEW_CATEGORY";
pub const FAST_PATH_CONFIDENCE_THRESHOLD: f64 = 0.85;
pub const NOVELTY_CONFIDENCE_THRESHOLD: f64 = 0.70;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SchemaDescriptor {
    pub id: Uuid,
    pub namespace: String,
    pub name: String,
    pub qualified_name: String,
    pub description: String,
    pub json_schema: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SchemaClassificationResult {
    Existing {
        schema: SchemaDescriptor,
        confidence: f64,
    },
    Novel {
        suggested_category: Option<String>,
        confidence: f64,
        reason: &'static str,
    },
}

#[derive(Clone)]
pub struct SchemaClassifier {
    jev: JevClient,
    db: Db,
}

impl SchemaClassifier {
    pub fn new(jev: JevClient, db: Db) -> Self {
        Self { jev, db }
    }

    pub async fn load_user_schemas(
        &self,
        user_id: Uuid,
    ) -> Result<Vec<SchemaDescriptor>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT id, namespace, name, description, json_schema \
             FROM data_schemas \
             WHERE (user_id = $1 OR user_id IS NULL) \
             ORDER BY namespace ASC, name ASC, version DESC",
        )
        .bind(user_id)
        .fetch_all(self.db.pool())
        .await?;

        let mut seen = std::collections::HashSet::new();
        let mut schemas = Vec::new();

        for r in rows {
            let ns: String = r.get("namespace");
            let name: String = r.get("name");
            let qname = format!("{ns}.{name}");
            if seen.contains(&qname) {
                continue;
            }
            seen.insert(qname.clone());
            schemas.push(SchemaDescriptor {
                id: r.get("id"),
                namespace: ns,
                name,
                qualified_name: qname,
                description: r.get("description"),
                json_schema: r.get("json_schema"),
            });
        }
        Ok(schemas)
    }

    pub async fn classify(
        &self,
        user_id: Uuid,
        payload: &Value,
    ) -> Result<SchemaClassificationResult, JevError> {
        let schemas = self.load_user_schemas(user_id).await.map_err(|e| {
            JevError::Api(
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                format!("Database error loading schemas: {e}"),
            )
        })?;

        if schemas.is_empty() {
            return Ok(SchemaClassificationResult::Novel {
                suggested_category: None,
                confidence: 1.0,
                reason: "no_registered_schemas",
            });
        }

        let mut options: Vec<(String, Option<String>)> = schemas
            .iter()
            .map(|s| (s.qualified_name.clone(), Some(s.description.clone())))
            .collect();

        options.push((
            NOVEL_CATEGORY_SENTINEL.to_string(),
            Some("Data does not fit any of the listed schema categories and requires a new schema definition".to_string()),
        ));

        let instructions = "Classify this data payload to the single most appropriate registered schema, or select UNKNOWN_NEW_CATEGORY if it represents a new domain.";
        let options_refs: Vec<(&str, Option<&str>)> = options
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_deref()))
            .collect();

        let (choice, confidence, _) = self
            .jev
            .choice(payload.clone(), instructions, &options_refs)
            .await?;

        tracing::info!(
            choice = %choice,
            confidence = confidence,
            "Jev Schema Classification completed"
        );

        if choice == NOVEL_CATEGORY_SENTINEL {
            return Ok(SchemaClassificationResult::Novel {
                suggested_category: None,
                confidence,
                reason: "classified_as_novel",
            });
        }

        if confidence >= FAST_PATH_CONFIDENCE_THRESHOLD
            && let Some(matched_schema) = schemas.into_iter().find(|s| s.qualified_name == choice)
        {
            return Ok(SchemaClassificationResult::Existing {
                schema: matched_schema,
                confidence,
            });
        }

        Ok(SchemaClassificationResult::Novel {
            suggested_category: Some(choice),
            confidence,
            reason: if confidence < NOVELTY_CONFIDENCE_THRESHOLD {
                "low_confidence"
            } else {
                "sub_threshold_ambiguity"
            },
        })
    }
}
