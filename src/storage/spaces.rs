use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::domain::spaces::{
    NodeState, RunState, Space, SpaceEdge, SpaceGraph, SpaceMessage, SpaceNode, SpaceState,
};

const SPACE_COLUMNS: &str = "id, user_id, title, intent, state, agent_spec, committed_collection_id, run_state, run_error, created_at, updated_at";

#[derive(Clone)]
pub struct SpaceRepository {
    pool: PgPool,
}

impl SpaceRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn create_space(
        &self,
        user_id: Uuid,
        title: &str,
        intent: &str,
        state: SpaceState,
        agent_spec: serde_json::Value,
    ) -> Result<Space, sqlx::Error> {
        let query = format!(
            r#"
            INSERT INTO spaces (user_id, title, intent, state, agent_spec)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING {SPACE_COLUMNS}
            "#
        );
        let row = sqlx::query(&query)
            .bind(user_id)
            .bind(title)
            .bind(intent)
            .bind(state.as_str())
            .bind(agent_spec)
            .fetch_one(&self.pool)
            .await?;

        Ok(map_space_row(row))
    }

    pub async fn create_workflow_space(
        &self,
        user_id: Uuid,
        title: &str,
        intent: &str,
    ) -> Result<Space, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let space = Self::create_workflow_space_in(&mut tx, user_id, title, intent).await?;
        tx.commit().await?;
        Ok(space)
    }

    pub async fn create_workflow_space_in(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
        user_id: Uuid,
        title: &str,
        intent: &str,
    ) -> Result<Space, sqlx::Error> {
        let query = format!(
            "INSERT INTO spaces(user_id,title,intent,state,agent_spec) VALUES($1,$2,$3,'ideating',$4) RETURNING {SPACE_COLUMNS}"
        );
        let row = sqlx::query(&query)
            .bind(user_id)
            .bind(title)
            .bind(intent)
            .bind(serde_json::json!({"workflow_version":2}))
            .fetch_one(&mut **tx)
            .await?;
        let space = map_space_row(row);
        sqlx::query("INSERT INTO space_nodes(space_id,kind,title,body,state) VALUES($1,'goal','Your vision',$2,'done')").bind(space.id).bind(intent).execute(&mut **tx).await?;
        sqlx::query("INSERT INTO jobs(kind,payload_reference_id) VALUES('run_space',$1)")
            .bind(space.id)
            .execute(&mut **tx)
            .await?;
        Ok(space)
    }

    pub async fn list_spaces(&self, user_id: Uuid) -> Result<Vec<Space>, sqlx::Error> {
        let query = format!(
            r#"
            SELECT {SPACE_COLUMNS}
            FROM spaces
            WHERE user_id = $1 AND state != 'dropped'
            ORDER BY created_at DESC
            "#
        );
        let rows = sqlx::query(&query)
            .bind(user_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(rows.into_iter().map(map_space_row).collect())
    }

    pub async fn get_space(
        &self,
        user_id: Uuid,
        space_id: Uuid,
    ) -> Result<Option<Space>, sqlx::Error> {
        let query = format!(
            r#"
            SELECT {SPACE_COLUMNS}
            FROM spaces
            WHERE user_id = $1 AND id = $2
            "#
        );
        let row = sqlx::query(&query)
            .bind(user_id)
            .bind(space_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(map_space_row))
    }

    pub async fn get_space_by_id_only(&self, space_id: Uuid) -> Result<Option<Space>, sqlx::Error> {
        let query = format!(
            r#"
            SELECT {SPACE_COLUMNS}
            FROM spaces
            WHERE id = $1
            "#
        );
        let row = sqlx::query(&query)
            .bind(space_id)
            .fetch_optional(&self.pool)
            .await?;

        Ok(row.map(map_space_row))
    }

    pub async fn set_run_state(
        &self,
        space_id: Uuid,
        run_state: RunState,
        run_error: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE spaces
            SET run_state = $1, run_error = $2, updated_at = now()
            WHERE id = $3
            "#,
        )
        .bind(run_state.as_str())
        .bind(run_error)
        .bind(space_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn add_message(
        &self,
        space_id: Uuid,
        role: &str,
        text: &str,
    ) -> Result<SpaceMessage, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO space_messages (space_id, role, text)
            VALUES ($1, $2, $3)
            RETURNING id, space_id, role, text, created_at
            "#,
        )
        .bind(space_id)
        .bind(role)
        .bind(text)
        .fetch_one(&self.pool)
        .await?;

        Ok(map_message_row(row))
    }

    pub async fn list_messages(&self, space_id: Uuid) -> Result<Vec<SpaceMessage>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, space_id, role, text, created_at
            FROM space_messages
            WHERE space_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(space_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_message_row).collect())
    }

    pub async fn update_space_state(
        &self,
        user_id: Uuid,
        space_id: Uuid,
        state: SpaceState,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE spaces
            SET state = $1, updated_at = now()
            WHERE user_id = $2 AND id = $3
            "#,
        )
        .bind(state.as_str())
        .bind(user_id)
        .bind(space_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn update_space_spec(
        &self,
        space_id: Uuid,
        agent_spec: serde_json::Value,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE spaces
            SET agent_spec = $1, updated_at = now()
            WHERE id = $2
            "#,
        )
        .bind(agent_spec)
        .bind(space_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn update_space_committed(
        &self,
        user_id: Uuid,
        space_id: Uuid,
        collection_id: Uuid,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE spaces
            SET state = 'committed', committed_collection_id = $1, updated_at = now()
            WHERE user_id = $2 AND id = $3
            "#,
        )
        .bind(collection_id)
        .bind(user_id)
        .bind(space_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn drop_space(&self, user_id: Uuid, space_id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE spaces
            SET state = 'dropped', updated_at = now()
            WHERE user_id = $1 AND id = $2
            "#,
        )
        .bind(user_id)
        .bind(space_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn add_node(
        &self,
        space_id: Uuid,
        kind: &str,
        title: &str,
        body: &str,
        data: serde_json::Value,
        state: NodeState,
        position: serde_json::Value,
        derived_from: &[Uuid],
        provenance: serde_json::Value,
    ) -> Result<SpaceNode, sqlx::Error> {
        let row = sqlx::query(
            r#"
            INSERT INTO space_nodes (
                space_id, kind, title, body, data, state, position, derived_from, provenance
            )
            VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            RETURNING id, space_id, kind, title, body, data, state, position, derived_from, provenance, version, created_at, updated_at
            "#,
        )
        .bind(space_id)
        .bind(kind)
        .bind(title)
        .bind(body)
        .bind(data)
        .bind(state.as_str())
        .bind(position)
        .bind(derived_from)
        .bind(provenance)
        .fetch_one(&self.pool)
        .await?;

        Ok(map_node_row(row))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_node(
        &self,
        space_id: Uuid,
        node_id: Uuid,
        title: Option<&str>,
        body: Option<&str>,
        data: Option<serde_json::Value>,
        state: Option<NodeState>,
        position: Option<serde_json::Value>,
        provenance: Option<serde_json::Value>,
    ) -> Result<Option<SpaceNode>, sqlx::Error> {
        let existing = self.get_node(space_id, node_id).await?;
        let Some(curr) = existing else {
            return Ok(None);
        };

        let new_title = title.unwrap_or(&curr.title);
        let new_body = body.unwrap_or(&curr.body);
        let new_data = data.unwrap_or(curr.data);
        let new_state = state.unwrap_or(curr.state);
        let new_position = position.unwrap_or(curr.position);
        let new_provenance = provenance.unwrap_or(curr.provenance);

        let row = sqlx::query(
            r#"
            UPDATE space_nodes
            SET title = $1, body = $2, data = $3, state = $4, position = $5,
                provenance = $6, version = version + 1, updated_at = now()
            WHERE space_id = $7 AND id = $8
            RETURNING id, space_id, kind, title, body, data, state, position, derived_from, provenance, version, created_at, updated_at
            "#,
        )
        .bind(new_title)
        .bind(new_body)
        .bind(new_data)
        .bind(new_state.as_str())
        .bind(new_position)
        .bind(new_provenance)
        .bind(space_id)
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_node_row))
    }

    pub async fn get_node(
        &self,
        space_id: Uuid,
        node_id: Uuid,
    ) -> Result<Option<SpaceNode>, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT id, space_id, kind, title, body, data, state, position, derived_from, provenance, version, created_at, updated_at
            FROM space_nodes
            WHERE space_id = $1 AND id = $2
            "#,
        )
        .bind(space_id)
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_node_row))
    }

    pub async fn list_nodes(&self, space_id: Uuid) -> Result<Vec<SpaceNode>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, space_id, kind, title, body, data, state, position, derived_from, provenance, version, created_at, updated_at
            FROM space_nodes
            WHERE space_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(space_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_node_row).collect())
    }

    pub async fn remove_node(&self, space_id: Uuid, node_id: Uuid) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            DELETE FROM space_nodes
            WHERE space_id = $1 AND id = $2
            "#,
        )
        .bind(space_id)
        .bind(node_id)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn mark_descendants_stale(
        &self,
        space_id: Uuid,
        node_id: Uuid,
    ) -> Result<Vec<Uuid>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            WITH RECURSIVE downstream AS (
                SELECT id FROM space_nodes
                WHERE space_id = $1 AND ($2 = ANY(derived_from))
                UNION
                SELECT sn.id FROM space_nodes sn
                INNER JOIN space_edges se ON se.to_node = sn.id
                INNER JOIN downstream d ON se.from_node = d.id
                WHERE sn.space_id = $1
            )
            UPDATE space_nodes
            SET state = 'stale', updated_at = now()
            WHERE space_id = $1 AND id IN (SELECT id FROM downstream)
            RETURNING id
            "#,
        )
        .bind(space_id)
        .bind(node_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(|r| r.get("id")).collect())
    }

    pub async fn add_edge(
        &self,
        space_id: Uuid,
        from_node: Uuid,
        to_node: Uuid,
    ) -> Result<SpaceEdge, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM spaces WHERE id=$1 FOR UPDATE")
            .bind(space_id)
            .fetch_one(&mut *tx)
            .await?;
        let cycle: bool = sqlx::query_scalar("WITH RECURSIVE descendants(id) AS (SELECT $3::uuid UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=$2)").bind(space_id).bind(from_node).bind(to_node).fetch_one(&mut *tx).await?;
        if cycle {
            return Err(sqlx::Error::Protocol(
                "Edges must preserve the dependency hierarchy".into(),
            ));
        }
        let row = sqlx::query(
            r#"
            INSERT INTO space_edges (space_id, from_node, to_node)
            SELECT $1, a.id, b.id
            FROM space_nodes a, space_nodes b
            WHERE a.id = $2 AND a.space_id = $1 AND b.id = $3 AND b.space_id = $1
            ON CONFLICT (space_id, from_node, to_node) DO UPDATE
                SET space_id = EXCLUDED.space_id
            RETURNING id, space_id, from_node, to_node, created_at
            "#,
        )
        .bind(space_id)
        .bind(from_node)
        .bind(to_node)
        .fetch_one(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(map_edge_row(row))
    }

    pub async fn finish_running_nodes(&self, space_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE space_nodes SET state = 'done', updated_at = now() WHERE space_id = $1 AND state = 'running'",
        )
        .bind(space_id)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    pub async fn remove_edge(
        &self,
        space_id: Uuid,
        from_node: Uuid,
        to_node: Uuid,
    ) -> Result<bool, sqlx::Error> {
        let result = sqlx::query(
            r#"
            DELETE FROM space_edges
            WHERE space_id = $1 AND from_node = $2 AND to_node = $3
            "#,
        )
        .bind(space_id)
        .bind(from_node)
        .bind(to_node)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected() > 0)
    }

    pub async fn list_edges(&self, space_id: Uuid) -> Result<Vec<SpaceEdge>, sqlx::Error> {
        let rows = sqlx::query(
            r#"
            SELECT id, space_id, from_node, to_node, created_at
            FROM space_edges
            WHERE space_id = $1
            ORDER BY created_at ASC
            "#,
        )
        .bind(space_id)
        .fetch_all(&self.pool)
        .await?;

        Ok(rows.into_iter().map(map_edge_row).collect())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_node_scoped(
        &self,
        user_id: Uuid,
        space_id: Uuid,
        node_id: Uuid,
        title: Option<&str>,
        body: Option<&str>,
        data: Option<serde_json::Value>,
        state: Option<NodeState>,
        position: Option<serde_json::Value>,
        provenance: Option<serde_json::Value>,
    ) -> Result<Option<SpaceNode>, sqlx::Error> {
        let existing = self.get_node(space_id, node_id).await?;
        let Some(curr) = existing else {
            return Ok(None);
        };

        let new_title = title.unwrap_or(&curr.title);
        let new_body = body.unwrap_or(&curr.body);
        let new_data = data.unwrap_or(curr.data);
        let new_state = state.unwrap_or(curr.state);
        let new_position = position.unwrap_or(curr.position);
        let new_provenance = provenance.unwrap_or(curr.provenance);

        let row = sqlx::query(
            r#"
            UPDATE space_nodes sn
            SET title = $1, body = $2, data = $3, state = $4, position = $5,
                provenance = $6, version = sn.version + 1, updated_at = now()
            FROM spaces s
            WHERE sn.space_id = s.id AND s.user_id = $7 AND sn.space_id = $8 AND sn.id = $9
            RETURNING sn.id, sn.space_id, sn.kind, sn.title, sn.body, sn.data, sn.state, sn.position, sn.derived_from, sn.provenance, sn.version, sn.created_at, sn.updated_at
            "#,
        )
        .bind(new_title)
        .bind(new_body)
        .bind(new_data)
        .bind(new_state.as_str())
        .bind(new_position)
        .bind(new_provenance)
        .bind(user_id)
        .bind(space_id)
        .bind(node_id)
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(map_node_row))
    }

    pub async fn get_graph(
        &self,
        user_id: Uuid,
        space_id: Uuid,
    ) -> Result<Option<SpaceGraph>, sqlx::Error> {
        let Some(space) = self.get_space(user_id, space_id).await? else {
            return Ok(None);
        };
        let nodes = self.list_nodes(space_id).await?;
        let edges = self.list_edges(space_id).await?;
        Ok(Some(SpaceGraph {
            space,
            nodes,
            edges,
        }))
    }
}

fn map_space_row(row: sqlx::postgres::PgRow) -> Space {
    let state_raw: String = row.get("state");
    let run_state_raw: String = row.get("run_state");
    Space {
        id: row.get("id"),
        user_id: row.get("user_id"),
        title: row.get("title"),
        intent: row.get("intent"),
        state: SpaceState::parse(&state_raw).unwrap_or(SpaceState::Ideating),
        agent_spec: row.get("agent_spec"),
        committed_collection_id: row.get("committed_collection_id"),
        run_state: RunState::parse(&run_state_raw).unwrap_or(RunState::Idle),
        run_error: row.get("run_error"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn map_node_row(row: sqlx::postgres::PgRow) -> SpaceNode {
    let state_raw: String = row.get("state");
    SpaceNode {
        id: row.get("id"),
        space_id: row.get("space_id"),
        kind: row.get("kind"),
        title: row.get("title"),
        body: row.get("body"),
        data: row.get("data"),
        state: NodeState::parse(&state_raw).unwrap_or(NodeState::Done),
        position: row.get("position"),
        derived_from: row.get("derived_from"),
        provenance: row.get("provenance"),
        version: row.get("version"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

fn map_edge_row(row: sqlx::postgres::PgRow) -> SpaceEdge {
    SpaceEdge {
        id: row.get("id"),
        space_id: row.get("space_id"),
        from_node: row.get("from_node"),
        to_node: row.get("to_node"),
        created_at: row.get("created_at"),
    }
}

fn map_message_row(row: sqlx::postgres::PgRow) -> SpaceMessage {
    SpaceMessage {
        id: row.get("id"),
        space_id: row.get("space_id"),
        role: row.get("role"),
        text: row.get("text"),
        created_at: row.get("created_at"),
    }
}
