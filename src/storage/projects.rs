use crate::domain::projects::{Project, ProjectData};
use sqlx::{PgPool, Row, postgres::PgRow};
use uuid::Uuid;

#[derive(Clone)]
pub struct ProjectRepository(pub PgPool);

fn map(row: PgRow) -> Result<Project, sqlx::Error> {
    let data =
        serde_json::from_value(row.get("data")).map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
    Ok(Project {
        id: row.get("id"),
        name: row.get("name"),
        data,
        revision: row.get("revision"),
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    })
}

impl ProjectRepository {
    pub async fn get(&self, user: Uuid, id: Uuid) -> Result<Option<Project>, sqlx::Error> {
        sqlx::query("SELECT * FROM projects WHERE user_id=$1 AND id=$2 AND NOT archived")
            .bind(user)
            .bind(id)
            .fetch_optional(&self.0)
            .await?
            .map(map)
            .transpose()
    }
    pub async fn list(
        &self,
        user: Uuid,
        before: Option<Uuid>,
    ) -> Result<Vec<Project>, sqlx::Error> {
        sqlx::query("SELECT id,user_id,name,(data - 'nodes' - 'messages' - 'activity') AS data,revision,created_at,updated_at FROM projects WHERE user_id=$1 AND NOT archived AND ($2::uuid IS NULL OR (updated_at,id)<(SELECT updated_at,id FROM projects WHERE id=$2 AND user_id=$1)) ORDER BY updated_at DESC,id DESC LIMIT 50").bind(user).bind(before).fetch_all(&self.0).await?.into_iter().map(map).collect()
    }
    pub async fn create(
        &self,
        user: Uuid,
        name: &str,
        data: &ProjectData,
    ) -> Result<Project, sqlx::Error> {
        map(
            sqlx::query("INSERT INTO projects(user_id,name,data) VALUES($1,$2,$3) RETURNING *")
                .bind(user)
                .bind(name)
                .bind(sqlx::types::Json(data))
                .fetch_one(&self.0)
                .await?,
        )
    }
    pub async fn update(
        &self,
        user: Uuid,
        id: Uuid,
        revision: i64,
        name: &str,
        data: &ProjectData,
    ) -> Result<Option<Project>, sqlx::Error> {
        sqlx::query("UPDATE projects SET name=$4,data=$5,revision=revision+1,updated_at=now() WHERE user_id=$1 AND id=$2 AND revision=$3 AND NOT archived RETURNING *").bind(user).bind(id).bind(revision).bind(name).bind(sqlx::types::Json(data)).fetch_optional(&self.0).await?.map(map).transpose()
    }
}
