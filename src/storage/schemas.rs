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
            RETURNING id, user_id, owner_scope, namespace, name, version, description, json_schema, color_token, icon_token, state, created_at, updated_at
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

        Ok(map_row(row))
    }

    pub async fn get_by_name(
        &self,
        user_id: Option<Uuid>,
        namespace: &str,
        name: &str,
    ) -> Result<Option<DataSchema>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, owner_scope, namespace, name, version, description, json_schema, color_token, icon_token, state, created_at, updated_at
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

        Ok(row.map(map_row))
    }

    pub async fn get_by_id(&self, id: Uuid) -> Result<Option<DataSchema>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, user_id, owner_scope, namespace, name, version, description, json_schema, color_token, icon_token, state, created_at, updated_at
            FROM data_schemas
            WHERE id = $1
            "#,
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_row))
    }
}

fn map_row(row: sqlx::postgres::PgRow) -> DataSchema {
    let state_str: String = row.get("state");
    let owner_scope: Option<String> = row.get("owner_scope");
    DataSchema {
        id: row.get("id"),
        user_id: row.get("user_id"),
        owner_scope: owner_scope.unwrap_or_else(|| "global".to_string()),
        namespace: row.get("namespace"),
        name: row.get("name"),
        version: row.get("version"),
        description: row.get("description"),
        json_schema: row.get("json_schema"),
        color_token: row.get("color_token"),
        icon_token: row.get("icon_token"),
        state: match state_str.as_str() {
            "deprecated" => SchemaState::Deprecated,
            _ => SchemaState::Active,
        },
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}
