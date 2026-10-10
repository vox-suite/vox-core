use crate::storage::spaces::SpaceRepository;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("Resource not found or not owned by you")]
    NotFound,
    #[error("Space is read-only or the node changed; refresh before retrying")]
    Conflict,
    #[error("{0}")]
    Invalid(&'static str),
    #[error("Resource write failed")]
    Database(#[from] sqlx::Error),
}
#[derive(Clone, Debug, Deserialize, Serialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ResourceRequest {
    pub command_id: Uuid,
    pub action: String,
    pub space_id: Option<Uuid>,
    pub arguments: Value,
}
#[derive(Clone)]
pub struct ResourceActions {
    pool: PgPool,
}
fn string<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a str, ActionError> {
    let text = v[key]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= max)
        .ok_or(ActionError::Invalid("Missing or oversized action argument"))?;
    Ok(text.trim())
}
fn id(v: &Value, key: &str) -> Result<Uuid, ActionError> {
    Uuid::parse_str(string(v, key, 36)?).map_err(|_| ActionError::Invalid("Invalid resource ID"))
}
impl ResourceActions {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn execute(&self, user: Uuid, input: ResourceRequest) -> Result<Value, ActionError> {
        if input.arguments.to_string().len() > 32768 {
            return Err(ActionError::Invalid("Action too large"));
        }
        let digest = hex::encode(Sha256::digest(
            serde_json::to_vec(&input).map_err(|_| ActionError::Invalid("Invalid action"))?,
        ));
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO desktop_action_receipts(user_id,command_id,request_digest) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(user).bind(input.command_id).bind(&digest).execute(&mut *tx).await?;
        let receipt=sqlx::query("SELECT request_digest,result FROM desktop_action_receipts WHERE user_id=$1 AND command_id=$2 FOR UPDATE").bind(user).bind(input.command_id).fetch_one(&mut *tx).await?;
        if receipt.get::<String, _>("request_digest") != digest {
            return Err(ActionError::Conflict);
        }
        if let Some(result) = receipt.get::<Option<Value>, _>("result") {
            tx.commit().await?;
            return Ok(result);
        }
        let args = &input.arguments;
        let output = if input.action == "create_space" {
            let intent = string(args, "intent", 8192)?;
            let title = args["title"].as_str().unwrap_or("New space");
            if title.len() > 200 {
                return Err(ActionError::Invalid("Title too long"));
            }
            let space =
                SpaceRepository::create_workflow_space_in(&mut tx, user, title, intent).await?;
            json!({"space_id":space.id,"space":space})
        } else {
            let space = input
                .space_id
                .ok_or(ActionError::Invalid("Choose a space"))?;
            let row = sqlx::query("SELECT state FROM spaces WHERE id=$1 AND user_id=$2 FOR UPDATE")
                .bind(space)
                .bind(user)
                .fetch_optional(&mut *tx)
                .await?
                .ok_or(ActionError::NotFound)?;
            if matches!(
                row.get::<String, _>("state").as_str(),
                "committed" | "dropped"
            ) {
                return Err(ActionError::Conflict);
            }
            let mut node_id = None;
            match input.action.as_str() {
                "space_chat" => {
                    let message = string(args, "message", 8192)?;
                    let node = args["nodeId"]
                        .as_str()
                        .map(Uuid::parse_str)
                        .transpose()
                        .map_err(|_| ActionError::Invalid("Invalid node ID"))?;
                    if let Some(node) = node {
                        let exists: bool = sqlx::query_scalar(
                            "SELECT EXISTS(SELECT 1 FROM space_nodes WHERE id=$1 AND space_id=$2)",
                        )
                        .bind(node)
                        .bind(space)
                        .fetch_one(&mut *tx)
                        .await?;
                        if !exists {
                            return Err(ActionError::NotFound);
                        }
                    }
                    let workflow:bool=sqlx::query_scalar("SELECT coalesce(agent_spec->>'workflow_version'='2',false) FROM spaces WHERE id=$1").bind(space).fetch_one(&mut *tx).await?;
                    if !workflow {
                        return Err(ActionError::Invalid(
                            "This space needs the current workflow before voice planning",
                        ));
                    }
                    sqlx::query("INSERT INTO space_workflow_requests(space_id,message,node_id,generation) SELECT id,$2,$3,workflow_generation FROM spaces WHERE id=$1").bind(space).bind(message).bind(node).execute(&mut *tx).await?;
                    sqlx::query(
                        "INSERT INTO space_messages(space_id,role,text) VALUES($1,'user',$2)",
                    )
                    .bind(space)
                    .bind(message)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO jobs(kind,payload_reference_id) VALUES('run_space',$1)",
                    )
                    .bind(space)
                    .execute(&mut *tx)
                    .await?;
                }
                "add_node" => {
                    let title = string(args, "title", 200)?;
                    let kind = string(args, "kind", 40)?;
                    let body = args["body"].as_str().unwrap_or("");
                    if body.len() > 16000 {
                        return Err(ActionError::Invalid("Node body too long"));
                    }
                    let node:Uuid=sqlx::query_scalar("INSERT INTO space_nodes(space_id,kind,title,body,state,provenance) VALUES($1,$2,$3,$4,'done',$5) RETURNING id").bind(space).bind(kind).bind(title).bind(body).bind(json!({"source":"user_voice_action","command_id":input.command_id})).fetch_one(&mut *tx).await?;
                    node_id = Some(node);
                }
                "update_node" | "remove_node" => {
                    let node = id(args, "nodeId")?;
                    let version = args["expectedVersion"]
                        .as_i64()
                        .ok_or(ActionError::Invalid("Read the node version before editing"))?;
                    let current: Option<i32> = sqlx::query_scalar(
                        "SELECT version FROM space_nodes WHERE space_id=$1 AND id=$2 FOR UPDATE",
                    )
                    .bind(space)
                    .bind(node)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if current.is_none() {
                        return Err(ActionError::NotFound);
                    }
                    if current != i32::try_from(version).ok() {
                        return Err(ActionError::Conflict);
                    }
                    invalidate_dependents(&mut tx, space, node).await?;
                    if input.action == "remove_node" {
                        sqlx::query("DELETE FROM space_nodes WHERE space_id=$1 AND id=$2")
                            .bind(space)
                            .bind(node)
                            .execute(&mut *tx)
                            .await?;
                    } else {
                        let title = args["title"].as_str();
                        let body = args["body"].as_str();
                        if title.is_none() && body.is_none() {
                            return Err(ActionError::Invalid("Specify a title or body"));
                        }
                        if title.is_some_and(|s| s.trim().is_empty() || s.len() > 200)
                            || body.is_some_and(|s| s.len() > 16000)
                        {
                            return Err(ActionError::Invalid("Invalid node content"));
                        }
                        sqlx::query("UPDATE space_nodes SET title=coalesce($3,title),body=coalesce($4,body),version=version+1,updated_at=now() WHERE space_id=$1 AND id=$2").bind(space).bind(node).bind(title).bind(body).execute(&mut *tx).await?;
                        sqlx::query("UPDATE space_tasks SET status='done',lease_token=NULL,expanded=false,error=NULL WHERE space_id=$1 AND node_id=$2").bind(space).bind(node).execute(&mut *tx).await?;
                        node_id = Some(node);
                    }
                }
                "connect_nodes" => {
                    let from = id(args, "fromNodeId")?;
                    let to = id(args, "toNodeId")?;
                    let count: i64 = sqlx::query_scalar(
                        "SELECT count(*) FROM space_nodes WHERE space_id=$1 AND id=ANY($2)",
                    )
                    .bind(space)
                    .bind(vec![from, to])
                    .fetch_one(&mut *tx)
                    .await?;
                    if count != 2 {
                        return Err(ActionError::NotFound);
                    }
                    let cycle:bool=sqlx::query_scalar("WITH RECURSIVE descendants(id) AS (SELECT $3::uuid UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) SELECT EXISTS(SELECT 1 FROM descendants WHERE id=$2)").bind(space).bind(from).bind(to).fetch_one(&mut *tx).await?;
                    if cycle {
                        return Err(ActionError::Invalid("Connection would create a cycle"));
                    }
                    sqlx::query("INSERT INTO space_edges(space_id,from_node,to_node) VALUES($1,$2,$3) ON CONFLICT DO NOTHING").bind(space).bind(from).bind(to).execute(&mut *tx).await?;
                    invalidate_dependents(&mut tx, space, from).await?;
                }
                "arrange_canvas" => {
                    let nodes = sqlx::query(
                        "SELECT id FROM space_nodes WHERE space_id=$1 ORDER BY created_at,id",
                    )
                    .bind(space)
                    .fetch_all(&mut *tx)
                    .await?;
                    for (i, node) in nodes.iter().enumerate() {
                        sqlx::query("UPDATE space_nodes SET position=$3,version=version+1 WHERE space_id=$1 AND id=$2").bind(space).bind(node.get::<Uuid,_>("id")).bind(json!({"x":(i%3)*300,"y":(i/3)*200})).execute(&mut *tx).await?;
                    }
                }
                "stop_work" => {
                    sqlx::query("UPDATE spaces SET run_state='idle',workflow_generation=workflow_generation+1 WHERE id=$1").bind(space).execute(&mut *tx).await?;
                    sqlx::query("UPDATE space_tasks SET status='cancelled',lease_token=NULL,error='Stopped by user' WHERE space_id=$1 AND status IN ('queued','running')").bind(space).execute(&mut *tx).await?;
                }
                _ => return Err(ActionError::Invalid("Unsupported resource action")),
            }
            json!({"space_id":space,"node_id":node_id})
        };
        sqlx::query(
            "UPDATE desktop_action_receipts SET result=$3 WHERE user_id=$1 AND command_id=$2",
        )
        .bind(user)
        .bind(input.command_id)
        .bind(&output)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(output)
    }
}
async fn invalidate_dependents(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    space: Uuid,
    node: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query("WITH RECURSIVE descendants(id) AS (SELECT e.to_node FROM space_edges e WHERE e.space_id=$1 AND e.from_node=$2 UNION SELECT e.to_node FROM space_edges e JOIN descendants d ON e.from_node=d.id WHERE e.space_id=$1) UPDATE space_nodes SET state='stale',version=version+1 WHERE space_id=$1 AND id IN(SELECT id FROM descendants)").bind(space).bind(node).execute(&mut **tx).await?;
    sqlx::query("UPDATE space_tasks SET status='cancelled',lease_token=NULL,expanded=true,error='Prerequisite changed; retry this branch' WHERE space_id=$1 AND node_id IN(SELECT id FROM space_nodes WHERE space_id=$1 AND state='stale')").bind(space).execute(&mut **tx).await?;
    Ok(())
}
