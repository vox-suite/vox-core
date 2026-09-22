/**
* Storage repository for JSON schema definitions and versioning.
*/
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::schemas::{DataSchema, SchemaState};

#[derive(Clone)]
pub struct SchemaRepository {
    pool: PgPool,
}

impl SchemaRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn create_version(
        &self,
        user_id: Option<Uuid>,
        namespace: &str,
        name: &str,
        version: i32,
        description: &str,
        json_schema: serde_json::Value,
    ) -> Result<DataSchema, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO data_schemas (user_id, namespace, name, version, description, json_schema)
            VALUES ($1, $2, $3, $4, $5, $6)
            RETURNING id, user_id, owner_scope, namespace, name, version, description, json_schema, state, created_at, updated_at
            "#,
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .bind(version)
        .bind(description)
        .bind(json_schema)
        .fetch_one(&self.pool)
        .await?;

        let state_str: String = row.get("state");
        let owner_scope: Option<String> = row.get("owner_scope");

        Ok(DataSchema {
            id: row.get("id"),
            user_id: row.get("user_id"),
            owner_scope: owner_scope.unwrap_or_else(|| "global".to_string()),
            namespace: row.get("namespace"),
            name: row.get("name"),
            version: row.get("version"),
            description: row.get("description"),
            json_schema: row.get("json_schema"),
            state: match state_str.as_str() {
                "deprecated" => SchemaState::Deprecated,
                _ => SchemaState::Active,
            },
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        })
    }

    pub async fn get_by_name(
        &self,
        user_id: Option<Uuid>,
        namespace: &str,
        name: &str,
    ) -> Result<Option<DataSchema>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, owner_scope, namespace, name, version, description, json_schema, state, created_at, updated_at
            FROM data_schemas
            WHERE (user_id = $1 OR user_id IS NULL)
              AND namespace = $2
              AND name = $3
              AND state = 'active'
            ORDER BY user_id NULLS LAST, version DESC
            LIMIT 1
            "#,
        )
        .bind(user_id)
        .bind(namespace)
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let state_str: String = r.get("state");
            let owner_scope: Option<String> = r.get("owner_scope");
            DataSchema {
                id: r.get("id"),
                user_id: r.get("user_id"),
                owner_scope: owner_scope.unwrap_or_else(|| "global".to_string()),
                namespace: r.get("namespace"),
                name: r.get("name"),
                version: r.get("version"),
                description: r.get("description"),
                json_schema: r.get("json_schema"),
                state: match state_str.as_str() {
                    "deprecated" => SchemaState::Deprecated,
                    _ => SchemaState::Active,
                },
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }
    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<DataSchema>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, owner_scope, namespace, name, version, description, json_schema, state, created_at, updated_at
            FROM data_schemas
            WHERE id = 
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            let state_str: String = r.get("state");
            let owner_scope: Option<String> = r.get("owner_scope");
            DataSchema {
                id: r.get("id"),
                user_id: r.get("user_id"),
                owner_scope: owner_scope.unwrap_or_else(|| "global".to_string()),
                namespace: r.get("namespace"),
                name: r.get("name"),
                version: r.get("version"),
                description: r.get("description"),
                json_schema: r.get("json_schema"),
                state: match state_str.as_str() {
                    "deprecated" => SchemaState::Deprecated,
                    _ => SchemaState::Active,
                },
                created_at: r.get("created_at"),
                updated_at: r.get("updated_at"),
            }
        }))
    }
}
