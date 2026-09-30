/**
* Agent tools for querying and mutating dynamic user records and collections.
*/
use crate::{db::Db, identity::UserId};
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
    NotConfigured,
}

impl fmt::Display for RecordToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(err) => write!(f, "database error: {err}"),
            Self::InvalidInput(msg) => write!(f, "invalid input: {msg}"),
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
