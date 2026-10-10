use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskProposal {
    pub role: String,
    pub title: String,
    pub brief: String,
    #[serde(default)]
    pub dependencies: Vec<Uuid>,
}
#[derive(Debug, Clone)]
pub struct ClaimedTask {
    pub node_id: Uuid,
    pub space_id: Uuid,
    pub role: String,
    pub brief: String,
    pub token: Uuid,
    pub generation: i32,
    pub attempt: i32,
}
#[derive(Clone)]
pub struct TaskRepository {
    pub pool: PgPool,
}
impl TaskRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn spawn(
        &self,
        space: Uuid,
        proposal: &TaskProposal,
        max_tasks: i64,
        max_children: i64,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        let generation: i32 =
            sqlx::query_scalar("SELECT workflow_generation FROM spaces WHERE id=$1")
                .bind(space)
                .fetch_one(&self.pool)
                .await?;
        self.spawn_generation(space, proposal, max_tasks, max_children, generation)
            .await
    }
    pub async fn spawn_generation(
        &self,
        space: Uuid,
        proposal: &TaskProposal,
        max_tasks: i64,
        max_children: i64,
        generation: i32,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        self.spawn_checked(space, proposal, max_tasks, max_children, generation, None)
            .await
    }
    pub async fn spawn_followup(
        &self,
        space: Uuid,
        proposal: &TaskProposal,
        max_tasks: i64,
        max_children: i64,
        generation: i32,
        parent: Uuid,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        self.spawn_checked(
            space,
            proposal,
            max_tasks,
            max_children,
            generation,
            Some(parent),
        )
        .await
    }
    async fn spawn_checked(
        &self,
        space: Uuid,
        proposal: &TaskProposal,
        max_tasks: i64,
        max_children: i64,
        expected_generation: i32,
        parent: Option<Uuid>,
    ) -> Result<Option<Uuid>, sqlx::Error> {
        if !valid_role(&proposal.role)
            || proposal.brief.trim().is_empty()
            || proposal.title.trim().is_empty()
            || proposal.dependencies.is_empty()
        {
            return Err(sqlx::Error::Protocol("Invalid task".into()));
        }
        let mut tx = self.pool.begin().await?;
        let row =
            sqlx::query("SELECT workflow_generation,state FROM spaces WHERE id=$1 FOR UPDATE")
                .bind(space)
                .fetch_one(&mut *tx)
                .await?;
        if matches!(
            row.get::<String, _>("state").as_str(),
            "dropped" | "committed"
        ) {
            return Ok(None);
        }
        let generation: i32 = row.get("workflow_generation");
        if generation != expected_generation {
            return Ok(None);
        }
        let cancelled: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM space_tasks WHERE node_id=ANY($1) AND status='cancelled')",
        )
        .bind(&proposal.dependencies)
        .fetch_one(&mut *tx)
        .await?;
        if cancelled {
            return Ok(None);
        }
        if let Some(parent) = parent {
            let valid:bool=sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM space_tasks t JOIN space_nodes n ON n.id=t.node_id WHERE t.node_id=$1 AND t.status='done' AND t.generation=$2 AND n.version=(t.input_versions->>t.node_id::text)::int+1)").bind(parent).bind(generation).fetch_one(&mut *tx).await?;
            if !valid {
                return Ok(None);
            }
        }
        let mut deps = proposal.dependencies.clone();
        deps.sort();
        deps.dedup();
        let key = format!(
            "{}:{}:{}",
            proposal.role,
            proposal.brief.trim(),
            deps.iter()
                .map(Uuid::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
        if let Some(id) = sqlx::query_scalar::<_, Uuid>(
            "SELECT node_id FROM space_tasks WHERE space_id=$1 AND generation=$2 AND dedupe_key=$3",
        )
        .bind(space)
        .bind(generation)
        .bind(&key)
        .fetch_optional(&mut *tx)
        .await?
        {
            return Ok(Some(id));
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM space_tasks WHERE space_id=$1 AND generation=$2",
        )
        .bind(space)
        .bind(generation)
        .fetch_one(&mut *tx)
        .await?;
        if count >= max_tasks {
            return Err(sqlx::Error::Protocol("Space task limit reached".into()));
        }
        let found: i64 =
            sqlx::query_scalar("SELECT count(*) FROM space_nodes WHERE space_id=$1 AND id=ANY($2)")
                .bind(space)
                .bind(&deps)
                .fetch_one(&mut *tx)
                .await?;
        if found != deps.len() as i64 {
            return Err(sqlx::Error::Protocol(
                "Dependencies must belong to this Space".into(),
            ));
        }
        for dep in &deps {
            let count: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM space_edges WHERE space_id=$1 AND from_node=$2",
            )
            .bind(space)
            .bind(dep)
            .fetch_one(&mut *tx)
            .await?;
            if count >= max_children {
                return Err(sqlx::Error::Protocol("Space branch limit reached".into()));
            }
        }
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO space_nodes(id,space_id,kind,title,body,data,state,derived_from) VALUES($1,$2,$3,$4,$5,$6,'running',$7)").bind(id).bind(space).bind(if proposal.role=="plan" {"plan"} else if proposal.role=="user_data" {"data"} else {"research"}).bind(&proposal.title).bind(&proposal.brief).bind(json!({"execution":{"role":proposal.role,"status":"queued"}})).bind(&deps).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO space_tasks(node_id,space_id,role,brief,dedupe_key,generation) VALUES($1,$2,$3,$4,$5,$6)").bind(id).bind(space).bind(&proposal.role).bind(&proposal.brief).bind(key).bind(generation).execute(&mut *tx).await?;
        for dep in deps {
            sqlx::query("INSERT INTO space_edges(space_id,from_node,to_node) VALUES($1,$2,$3)")
                .bind(space)
                .bind(dep)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(Some(id))
    }
    pub async fn claim(&self, space: Uuid) -> Result<Option<ClaimedTask>, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM spaces WHERE id=$1 FOR UPDATE")
            .bind(space)
            .fetch_one(&mut *tx)
            .await?;
        sqlx::query("UPDATE space_tasks SET status=CASE WHEN attempt>=2 THEN 'failed' ELSE 'queued' END, error='Worker lease expired',lease_token=NULL WHERE space_id=$1 AND status='running' AND lease_expires_at<now()").bind(space).execute(&mut *tx).await?;
        sqlx::query("UPDATE space_tasks t SET status='blocked',error='A prerequisite did not complete' WHERE t.space_id=$1 AND t.status='queued' AND EXISTS(SELECT 1 FROM space_edges e JOIN space_tasks p ON p.node_id=e.from_node WHERE e.to_node=t.node_id AND p.status IN ('failed','blocked','cancelled'))").bind(space).execute(&mut *tx).await?;
        let running: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM space_tasks WHERE space_id=$1 AND status='running'",
        )
        .bind(space)
        .fetch_one(&mut *tx)
        .await?;
        if running >= 3 {
            tx.commit().await?;
            return Ok(None);
        }
        let row=sqlx::query("SELECT t.* FROM space_tasks t JOIN spaces s ON s.id=t.space_id WHERE t.space_id=$1 AND t.status='queued' AND t.generation=s.workflow_generation AND s.state NOT IN ('committed','dropped') AND NOT EXISTS(SELECT 1 FROM space_edges e JOIN space_nodes n ON n.id=e.from_node LEFT JOIN space_tasks p ON p.node_id=n.id WHERE e.to_node=t.node_id AND (n.state!='done' OR (p.node_id IS NOT NULL AND p.status!='done'))) ORDER BY t.updated_at,t.node_id LIMIT 1 FOR UPDATE OF t SKIP LOCKED").bind(space).fetch_optional(&mut *tx).await?;
        let Some(row) = row else {
            tx.commit().await?;
            self.sync_metadata(space).await?;
            return Ok(None);
        };
        let id: Uuid = row.get("node_id");
        let token = Uuid::new_v4();
        let generation: i32 = row.get("generation");
        let attempt: i32 = row.get::<i32, _>("attempt") + 1;
        sqlx::query("UPDATE space_tasks SET status='running',attempt=attempt+1,lease_token=$2,lease_expires_at=now()+interval '90 seconds',input_versions=(SELECT coalesce(jsonb_object_agg(n.id::text,n.version),'{}') FROM space_edges e JOIN space_nodes n ON n.id=e.from_node WHERE e.to_node=$1) || (SELECT jsonb_build_object(id::text,version) FROM space_nodes WHERE id=$1),updated_at=now() WHERE node_id=$1").bind(id).bind(token).execute(&mut *tx).await?;
        tx.commit().await?;
        self.sync_metadata(space).await?;
        Ok(Some(ClaimedTask {
            node_id: id,
            space_id: space,
            role: row.get("role"),
            brief: row.get("brief"),
            token,
            generation,
            attempt,
        }))
    }
    pub async fn sync_metadata(&self, space: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query("UPDATE space_nodes n SET data=jsonb_set(n.data,'{execution}',jsonb_build_object('role',t.role,'status',t.status,'error',t.error,'attempt',t.attempt)),state=CASE WHEN t.status='done' THEN 'done' WHEN t.status IN ('failed','blocked','cancelled') THEN 'rejected' ELSE 'running' END FROM space_tasks t WHERE n.id=t.node_id AND t.space_id=$1").bind(space).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn heartbeat(&self, task: &ClaimedTask) -> Result<bool, sqlx::Error> {
        Ok(sqlx::query("UPDATE space_tasks SET lease_expires_at=now()+interval '90 seconds' WHERE node_id=$1 AND lease_token=$2 AND status='running'").bind(task.node_id).bind(task.token).execute(&self.pool).await?.rows_affected()>0)
    }
    pub async fn finish(
        &self,
        task: &ClaimedTask,
        output: &Value,
        success: bool,
    ) -> Result<bool, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM spaces WHERE id=$1 FOR UPDATE")
            .bind(task.space_id)
            .fetch_one(&mut *tx)
            .await?;
        let result=sqlx::query("UPDATE space_tasks t SET status=$3,output=$4,error=$5,lease_token=NULL,lease_expires_at=NULL,updated_at=now() FROM spaces s WHERE t.node_id=$1 AND t.lease_token=$2 AND t.status='running' AND s.id=t.space_id AND s.workflow_generation=t.generation AND (t.input_versions->>t.node_id::text)::int=(SELECT version FROM space_nodes WHERE id=t.node_id) AND NOT EXISTS(SELECT 1 FROM space_edges e JOIN space_nodes n ON n.id=e.from_node WHERE e.to_node=t.node_id AND (t.input_versions->>n.id::text)::int IS DISTINCT FROM n.version)").bind(task.node_id).bind(task.token).bind(if success {"done"} else if task.attempt<2 {"queued"} else {"failed"}).bind(output).bind(if success {None} else {Some(output.to_string())}).execute(&mut *tx).await?;
        if result.rows_affected() == 0 {
            sqlx::query("UPDATE space_tasks SET status='cancelled',error='Inputs changed while the task was running',lease_token=NULL WHERE node_id=$1 AND lease_token=$2").bind(task.node_id).bind(task.token).execute(&mut *tx).await?;
            tx.commit().await?;
            self.sync_metadata(task.space_id).await?;
            return Ok(false);
        }
        if success {
            sqlx::query("UPDATE space_nodes SET body=$2,provenance=$3,version=version+1,updated_at=now() WHERE id=$1").bind(task.node_id).bind(output["summary"].as_str().unwrap_or("Completed")).bind(output["evidence"].clone()).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        self.sync_metadata(task.space_id).await?;
        Ok(true)
    }
    pub async fn stop(&self, space: Uuid) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("UPDATE spaces SET workflow_generation=workflow_generation+1,run_state='idle',updated_at=now() WHERE id=$1").bind(space).execute(&mut *tx).await?;
        sqlx::query("UPDATE space_tasks SET status='cancelled',lease_token=NULL,lease_expires_at=NULL WHERE space_id=$1 AND status IN ('queued','running','blocked')").bind(space).execute(&mut *tx).await?;
        sqlx::query(
            "UPDATE space_workflow_requests SET processed=true WHERE space_id=$1 AND NOT processed",
        )
        .bind(space)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.sync_metadata(space).await
    }
    pub async fn retry(&self, space: Uuid, node: Uuid) -> Result<(), sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        let generation: i32 =
            sqlx::query_scalar("SELECT workflow_generation FROM spaces WHERE id=$1 FOR UPDATE")
                .bind(space)
                .fetch_one(&mut *tx)
                .await?;
        sqlx::query("WITH RECURSIVE descendants(id) AS(SELECT $2::uuid UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) UPDATE space_tasks SET status='queued',attempt=0,error=NULL,lease_token=NULL,expanded=false,generation=$3 WHERE space_id=$1 AND node_id IN(SELECT id FROM descendants) AND status IN ('failed','blocked','cancelled','done')").bind(space).bind(node).bind(generation).execute(&mut *tx).await?;
        sqlx::query("INSERT INTO jobs(kind,payload_reference_id) VALUES('run_space',$1)")
            .bind(space)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        self.sync_metadata(space).await
    }
}
pub fn valid_role(role: &str) -> bool {
    matches!(role, "web_search" | "user_data" | "synthesis" | "plan")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_scoped_worker_roles() {
        assert!(valid_role("web_search"));
        assert!(valid_role("user_data"));
        assert!(!valid_role("shell"));
    }
}
