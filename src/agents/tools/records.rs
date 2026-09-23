/**
* Agent tools for querying and mutating dynamic user records and collections.
*/
use crate::{db::Db, identity::UserId};
use chrono::{DateTime, Utc};
use rig::tool::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::Row;
use std::{error::Error as StdError, fmt};
use uuid::Uuid;

#[derive(Debug)]
pub enum RecordToolError {
    Database(sqlx::Error),
    InvalidInput(String),
    ValidationFailed(String),
    NotConfigured,
}

impl fmt::Display for RecordToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
            Self::ValidationFailed(msg) => write!(f, "type-safety validation failed: {msg}"),
            Self::NotConfigured => write!(f, "database is not configured"),
        }
    }
}

impl StdError for RecordToolError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        match self {
            Self::Database(err) => Some(err),
            _ => None,
        }
    }
}

impl From<sqlx::Error> for RecordToolError {
    fn from(err: sqlx::Error) -> Self {
        Self::Database(err)
    }
}

pub fn validate_data_against_schema(schema: &Value, data: &Value) -> Result<(), String> {
    let data_obj = match data.as_object() {
        Some(obj) => obj,
        None => return Err("record data payload must be a JSON object".to_string()),
    };

    if let Some(required) = schema.get("required").and_then(|r| r.as_array()) {
        for req_field in required {
            if let Some(field_name) = req_field.as_str()
                && (!data_obj.contains_key(field_name) || data_obj[field_name].is_null())
            {
                return Err(format!("missing required field '{field_name}'"));
            }
        }
    }

    if let Some(properties) = schema.get("properties").and_then(|p| p.as_object()) {
        for (field_name, field_spec) in properties {
            if let Some(val) = data_obj.get(field_name) {
                if val.is_null() {
                    continue;
                }
                if let Some(expected_type) = field_spec.get("type").and_then(|t| t.as_str()) {
                    let type_matches = match expected_type {
                        "string" => val.is_string(),
                        "number" => val.is_number(),
                        "integer" => {
                            val.is_i64()
                                || val.is_u64()
                                || val.as_f64().is_some_and(|f| f.fract() == 0.0)
                        }
                        "boolean" => val.is_boolean(),
                        "array" => val.is_array(),
                        "object" => val.is_object(),
                        _ => true,
                    };
                    if !type_matches {
                        return Err(format!(
                            "field '{field_name}' expected type '{expected_type}', got {}",
                            json_type_name(val)
                        ));
                    }
                }

                if let Some(enum_vals) = field_spec.get("enum").and_then(|e| e.as_array())
                    && !enum_vals.iter().any(|v| v == val)
                {
                    return Err(format!(
                        "field '{field_name}' with value '{val}' is not in allowed enum list"
                    ));
                }

                if let Some(num) = val.as_f64() {
                    if let Some(min) = field_spec.get("minimum").and_then(|m| m.as_f64())
                        && num < min
                    {
                        return Err(format!(
                            "field '{field_name}' ({num}) is less than minimum permitted ({min})"
                        ));
                    }
                    if let Some(max) = field_spec.get("maximum").and_then(|m| m.as_f64())
                        && num > max
                    {
                        return Err(format!(
                            "field '{field_name}' ({num}) exceeds maximum permitted ({max})"
                        ));
                    }
                }
            }
        }
    }

    Ok(())
}

fn json_type_name(val: &Value) -> &'static str {
    match val {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DefineDataSchemaArgs {
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub json_schema: Value,
}

#[derive(Clone)]
pub struct DefineDataSchema {
    db: Option<Db>,
    user_id: UserId,
}

impl DefineDataSchema {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for DefineDataSchema {
    const NAME: &'static str = "define_data_schema";
    type Args = DefineDataSchemaArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Register a new structured data schema (category/group) to store future historical data in a type-safe, validated way. Call this whenever a new kind of data arrives from a client that does not match existing schemas."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "High-level domain/namespace, e.g. finance, location, health, activity, smart_home, vehicle"
                },
                "name": {
                    "type": "string",
                    "description": "Specific entity name, e.g. expense, fuel_log, blood_pressure, coffee_brew, commute"
                },
                "description": {
                    "type": "string",
                    "description": "Clear semantic description of this data kind and when to use it"
                },
                "json_schema": {
                    "type": "object",
                    "description": "Standard JSON Schema object with 'properties', 'required', and field types (string, number, boolean, array, object)"
                }
            },
            "required": ["namespace", "name", "description", "json_schema"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let namespace = args.namespace.trim();
        let name = args.name.trim();
        let description = args.description.trim();

        if namespace.is_empty() || name.is_empty() || description.is_empty() {
            return Err(RecordToolError::InvalidInput(
                "namespace, name, and description must not be empty".into(),
            ));
        }

        if !args.json_schema.is_object() {
            return Err(RecordToolError::InvalidInput(
                "json_schema must be a valid JSON object specification".into(),
            ));
        }

        let current_version = sqlx::query_scalar::<_, i32>(
            "SELECT COALESCE(MAX(version), 0) FROM data_schemas WHERE user_id = $1 AND namespace = $2 AND name = $3",
        )
        .bind(self.user_id.0)
        .bind(namespace)
        .bind(name)
        .fetch_one(db.pool())
        .await?;

        let new_version = current_version + 1;

        let schema_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema) \
             VALUES ($1, $2, $3, $4, $5, $6) \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(namespace)
        .bind(name)
        .bind(new_version)
        .bind(description)
        .bind(&args.json_schema)
        .fetch_one(db.pool())
        .await?;

        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            schema_id = %schema_id,
            namespace = %namespace,
            name = %name,
            version = new_version,
            "Defined new data schema"
        );

        Ok(json!({
            "status": "defined",
            "schema_id": schema_id.to_string(),
            "namespace": namespace,
            "name": name,
            "version": new_version,
            "description": description,
            "json_schema": args.json_schema
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListDataSchemasArgs {
    pub namespace: Option<String>,
    pub query: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListDataSchemas {
    db: Option<Db>,
    user_id: UserId,
}

impl ListDataSchemas {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListDataSchemas {
    const NAME: &'static str = "list_data_schemas";
    type Args = ListDataSchemasArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "List all registered data schemas (both system-standard and user-defined) with their structure and field properties. Use this to find existing categories before storing or querying data."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "namespace": {
                    "type": "string",
                    "description": "Optional filter by namespace (e.g. finance, location, health, activity)"
                },
                "query": {
                    "type": "string",
                    "description": "Optional keyword search against name or description"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of schemas to return; defaults to 25"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let limit = args.limit.unwrap_or(25).clamp(1, 100);

        let rows = sqlx::query(
            "SELECT id, user_id, namespace, name, version, description, json_schema, created_at \
             FROM data_schemas \
             WHERE (user_id = $1 OR user_id IS NULL) \
               AND ($2::TEXT IS NULL OR namespace = $2) \
               AND ($3::TEXT IS NULL OR name ILIKE '%' || $3 || '%' OR description ILIKE '%' || $3 || '%') \
             ORDER BY namespace ASC, name ASC, version DESC \
             LIMIT $4",
        )
        .bind(self.user_id.0)
        .bind(args.namespace.as_deref())
        .bind(args.query.as_deref())
        .bind(limit)
        .fetch_all(db.pool())
        .await?;

        let schemas: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let uid: Option<Uuid> = r.get("user_id");
                let ns: String = r.get("namespace");
                let name: String = r.get("name");
                let version: i32 = r.get("version");
                let desc: String = r.get("description");
                let schema: Value = r.get("json_schema");
                json!({
                    "schema_id": id.to_string(),
                    "is_custom": uid.is_some(),
                    "namespace": ns,
                    "name": name,
                    "version": version,
                    "description": desc,
                    "json_schema": schema
                })
            })
            .collect();

        Ok(json!({ "schemas": schemas }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateUserRecordArgs {
    pub schema_id: Option<String>,
    pub namespace: Option<String>,
    pub schema_name: Option<String>,
    pub domain: Option<String>,
    pub entity_type: Option<String>,
    pub title: String,
    pub data: Option<Value>,
    pub occurred_at: Option<String>,
    pub source: Option<String>,
}

#[derive(Clone)]
pub struct CreateUserRecord {
    db: Option<Db>,
    user_id: UserId,
}

impl CreateUserRecord {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for CreateUserRecord {
    const NAME: &'static str = "create_user_record";
    type Args = CreateUserRecordArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Store a structured user record. Validates data against the target schema (using schema_id or namespace + schema_name). If a new kind of data arrives, use define_data_schema first."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema_id": {
                    "type": "string",
                    "description": "UUID of registered data schema to validate against and link to"
                },
                "namespace": {
                    "type": "string",
                    "description": "Namespace of schema (e.g. finance, location, health, activity, vehicle)"
                },
                "schema_name": {
                    "type": "string",
                    "description": "Name of registered schema (e.g. transaction, breadcrumb, vitals, log)"
                },
                "domain": {
                    "type": "string",
                    "description": "Fallback legacy domain if schema_id/namespace is not yet registered"
                },
                "entity_type": {
                    "type": "string",
                    "description": "Fallback entity type"
                },
                "title": {
                    "type": "string",
                    "description": "Short headline or title of the record"
                },
                "data": {
                    "type": "object",
                    "description": "Structured JSON payload adhering to the schema properties"
                },
                "occurred_at": {
                    "type": "string",
                    "description": "ISO 8601 timestamp of when the event occurred; defaults to now()"
                },
                "source": {
                    "type": "string",
                    "description": "Originating source (e.g. mobile_client, desktop_client, agent, voice)"
                }
            },
            "required": ["title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let title = args.title.trim();
        if title.is_empty() {
            return Err(RecordToolError::InvalidInput("title is required".into()));
        }

        let data_val = args.data.unwrap_or_else(|| json!({}));

        let mut target_schema_id: Option<Uuid> = None;
        let mut resolved_domain = args.domain.clone();
        let mut resolved_entity_type = args.entity_type.clone();

        if let Some(s_id_str) = &args.schema_id {
            if let Ok(parsed_id) = Uuid::parse_str(s_id_str.trim()) {
                let schema_row = sqlx::query(
                    "SELECT id, namespace, name, json_schema FROM data_schemas \
                     WHERE id = $1 AND (user_id = $2 OR user_id IS NULL)",
                )
                .bind(parsed_id)
                .bind(self.user_id.0)
                .fetch_optional(db.pool())
                .await?;

                if let Some(row) = schema_row {
                    let schema_json: Value = row.get("json_schema");
                    validate_data_against_schema(&schema_json, &data_val)
                        .map_err(RecordToolError::ValidationFailed)?;
                    target_schema_id = Some(row.get("id"));
                    resolved_domain = Some(row.get("namespace"));
                    resolved_entity_type = Some(row.get("name"));
                } else {
                    return Err(RecordToolError::InvalidInput(format!(
                        "specified schema_id '{s_id_str}' was not found"
                    )));
                }
            }
        } else if let (Some(ns), Some(s_name)) = (&args.namespace, &args.schema_name) {
            let schema_row = sqlx::query(
                "SELECT id, namespace, name, json_schema FROM data_schemas \
                 WHERE namespace = $1 AND name = $2 AND (user_id = $3 OR user_id IS NULL) \
                 ORDER BY (user_id IS NOT NULL) DESC, version DESC \
                 LIMIT 1",
            )
            .bind(ns.trim())
            .bind(s_name.trim())
            .bind(self.user_id.0)
            .fetch_optional(db.pool())
            .await?;

            if let Some(row) = schema_row {
                let schema_json: Value = row.get("json_schema");
                validate_data_against_schema(&schema_json, &data_val)
                    .map_err(RecordToolError::ValidationFailed)?;
                target_schema_id = Some(row.get("id"));
                resolved_domain = Some(row.get("namespace"));
                resolved_entity_type = Some(row.get("name"));
            }
        }

        let domain = resolved_domain.unwrap_or_else(|| "general".to_string());
        let entity_type = resolved_entity_type.unwrap_or_else(|| "record".to_string());
        let source = args.source.unwrap_or_else(|| "agent".to_string());

        let occurred_at = if let Some(ts_str) = args.occurred_at {
            DateTime::parse_from_rfc3339(&ts_str)
                .map(|dt| dt.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now())
        } else {
            Utc::now()
        };

        let Some(target_schema_id) = target_schema_id else {
            return Err(RecordToolError::InvalidInput(
                "a schema is required to store a record".into(),
            ));
        };

        let record_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO records (user_id, schema_id, schema_scope, kind, domain, entity_type, title, data, occurred_at, source) \
             SELECT $1, s.id, s.owner_scope, 'fact', $3, $4, $5, $6, $7, $8 \
             FROM data_schemas s \
             WHERE s.id = $2 AND (s.user_id IS NULL OR s.user_id = $1) \
             RETURNING records.id",
        )
        .bind(self.user_id.0)
        .bind(target_schema_id)
        .bind(&domain)
        .bind(&entity_type)
        .bind(title)
        .bind(&data_val)
        .bind(occurred_at)
        .bind(source)
        .fetch_one(db.pool())
        .await?;

        tracing::info!(
            tool = Self::NAME,
            user_id = %self.user_id.0,
            record_id = %record_id,
            schema_id = ?target_schema_id,
            domain = %domain,
            entity_type = %entity_type,
            "Stored structured user record"
        );

        Ok(json!({
            "status": "created",
            "record_id": record_id.to_string(),
            "schema_id": target_schema_id.to_string(),
            "domain": domain,
            "entity_type": entity_type,
            "title": title
        }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListUserRecordsArgs {
    pub schema_id: Option<String>,
    pub namespace: Option<String>,
    pub schema_name: Option<String>,
    pub domain: Option<String>,
    pub entity_type: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Clone)]
pub struct ListUserRecords {
    db: Option<Db>,
    user_id: UserId,
}

impl ListUserRecords {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ListUserRecords {
    const NAME: &'static str = "list_user_records";
    type Args = ListUserRecordsArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Retrieve stored user records. Filter by schema_id, namespace, schema_name, or legacy domain/entity_type."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema_id": {
                    "type": "string",
                    "description": "Filter by schema UUID"
                },
                "namespace": {
                    "type": "string",
                    "description": "Filter by namespace (e.g. finance, location, health, activity)"
                },
                "schema_name": {
                    "type": "string",
                    "description": "Filter by schema name (e.g. transaction, breadcrumb, vitals, log)"
                },
                "domain": {
                    "type": "string",
                    "description": "Optional legacy domain filter"
                },
                "entity_type": {
                    "type": "string",
                    "description": "Optional entity type filter"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of records to return; defaults to 15"
                }
            }
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let limit = args.limit.unwrap_or(15).clamp(1, 100);

        let parsed_schema_id = args
            .schema_id
            .as_deref()
            .and_then(|s| Uuid::parse_str(s.trim()).ok());

        let rows = sqlx::query(
            "SELECT r.id, r.schema_id, r.domain, r.entity_type, r.title, r.data, r.occurred_at, r.source, \
                    s.namespace AS schema_namespace, s.name AS schema_name \
             FROM records r \
             LEFT JOIN data_schemas s ON r.schema_id = s.id \
             WHERE r.user_id = $1 \
               AND ($2::UUID IS NULL OR r.schema_id = $2) \
               AND ($3::TEXT IS NULL OR r.domain = $3 OR s.namespace = $3) \
               AND ($4::TEXT IS NULL OR r.entity_type = $4 OR s.name = $4) \
             ORDER BY r.occurred_at DESC \
             LIMIT $5",
        )
        .bind(self.user_id.0)
        .bind(parsed_schema_id)
        .bind(args.namespace.as_deref().or(args.domain.as_deref()))
        .bind(args.schema_name.as_deref().or(args.entity_type.as_deref()))
        .bind(limit)
        .fetch_all(db.pool())
        .await?;

        let records: Vec<Value> = rows
            .into_iter()
            .map(|r| {
                let id: Uuid = r.get("id");
                let s_id: Option<Uuid> = r.get("schema_id");
                let dom: String = r.get("domain");
                let ent: String = r.get("entity_type");
                let title: String = r.get("title");
                let data: Value = r.get("data");
                let occurred_at: DateTime<Utc> = r.get("occurred_at");
                let src: String = r.get("source");
                let s_ns: Option<String> = r.get("schema_namespace");
                let s_name: Option<String> = r.get("schema_name");
                json!({
                    "id": id.to_string(),
                    "schema_id": s_id.map(|u| u.to_string()),
                    "namespace": s_ns.unwrap_or(dom.clone()),
                    "name": s_name.unwrap_or(ent.clone()),
                    "domain": dom,
                    "entity_type": ent,
                    "title": title,
                    "data": data,
                    "occurred_at": occurred_at.to_rfc3339(),
                    "source": src
                })
            })
            .collect();

        Ok(json!({ "records": records }))
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ManageUserGoalArgs {
    pub schema_id: Option<String>,
    pub domain: String,
    pub title: String,
    pub description: Option<String>,
    pub target_metric: Option<Value>,
    pub status: Option<String>,
}

#[derive(Clone)]
pub struct ManageUserGoal {
    db: Option<Db>,
    user_id: UserId,
}

impl ManageUserGoal {
    pub fn new(db: Option<Db>, user_id: UserId) -> Self {
        Self { db, user_id }
    }
}

impl Tool for ManageUserGoal {
    const NAME: &'static str = "manage_user_goal";
    type Args = ManageUserGoalArgs;
    type Output = Value;
    type Error = RecordToolError;

    fn description(&self) -> String {
        "Create or update personal targets, budgets, or goals tied to user data categories."
            .to_owned()
    }

    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "schema_id": {
                    "type": "string",
                    "description": "Optional data schema UUID associated with this goal"
                },
                "domain": {
                    "type": "string",
                    "description": "Domain of the goal: finance, health, work, habit"
                },
                "title": {
                    "type": "string",
                    "description": "Name of the goal, e.g. Limit dining spend"
                },
                "description": {
                    "type": "string",
                    "description": "Target description"
                },
                "target_metric": {
                    "type": "object",
                    "description": "Structured target metric, e.g. {\"limit_amount\": 400, \"currency\": \"USD\", \"period\": \"monthly\"}"
                },
                "status": {
                    "type": "string",
                    "enum": ["active", "paused", "completed", "abandoned"],
                    "description": "Status of the goal; defaults to active"
                }
            },
            "required": ["domain", "title"]
        })
    }

    async fn call(
        &self,
        _context: &mut rig::prelude::ToolContext,
        args: Self::Args,
    ) -> Result<Self::Output, Self::Error> {
        let db = self.db.as_ref().ok_or(RecordToolError::NotConfigured)?;
        let domain = args.domain.trim();
        let title = args.title.trim();
        let desc = args.description.as_deref().unwrap_or("").trim();
        let status = args.status.as_deref().unwrap_or("active");
        let metric = args.target_metric.unwrap_or_else(|| json!({}));
        let parsed_schema_id = args
            .schema_id
            .as_deref()
            .and_then(|s| Uuid::parse_str(s.trim()).ok());

        let id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO user_goals (user_id, schema_id, domain, title, description, target_metric, status) \
             VALUES ($1, $2, $3, $4, $5, $6, $7) \
             RETURNING id",
        )
        .bind(self.user_id.0)
        .bind(parsed_schema_id)
        .bind(domain)
        .bind(title)
        .bind(desc)
        .bind(&metric)
        .bind(status)
        .fetch_one(db.pool())
        .await?;

        Ok(json!({
            "status": "created",
            "goal_id": id.to_string(),
            "domain": domain,
            "title": title
        }))
    }
}
