use super::pulse::PulseRepository;
use crate::domain::{pulse::PulseDefinition, pulse_goals::*};
use chrono::NaiveDate;
use sqlx::Row;
use uuid::Uuid;

const COLUMNS: &str = "g.id,g.title,g.kind,g.direction,g.period,g.target,g.unit,g.definition,g.starts_on,g.deadline,g.created_at,\
 COALESCE((SELECT sum(e.amount) FROM pulse_goal_entries e WHERE e.goal_id=g.id),0)::float8 AS saved";

fn parse<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, sqlx::Error> {
    serde_json::from_value(serde_json::Value::String(text.into()))
        .map_err(|e| sqlx::Error::Decode(Box::new(e)))
}
fn row_to_goal(row: &sqlx::postgres::PgRow) -> Result<GoalRow, sqlx::Error> {
    let definition: Option<serde_json::Value> = row.get("definition");
    let period: Option<String> = row.get("period");
    Ok(GoalRow {
        id: row.get("id"),
        title: row.get("title"),
        kind: parse(row.get::<String, _>("kind").as_str())?,
        direction: parse(row.get::<String, _>("direction").as_str())?,
        period: period.as_deref().map(parse).transpose()?,
        target: row.get("target"),
        unit: row.get("unit"),
        definition: definition
            .map(serde_json::from_value::<PulseDefinition>)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
        starts_on: row.get("starts_on"),
        deadline: row.get("deadline"),
        created_at: row.get("created_at"),
        saved: row.get("saved"),
    })
}

impl PulseRepository {
    pub async fn list_goals(&self, user: Uuid) -> Result<Vec<GoalRow>, sqlx::Error> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM pulse_goals g WHERE g.user_id=$1 ORDER BY g.created_at,g.id"
        ))
        .bind(user)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(row_to_goal).collect()
    }
    pub async fn get_goal(&self, user: Uuid, id: Uuid) -> Result<Option<GoalRow>, sqlx::Error> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM pulse_goals g WHERE g.user_id=$1 AND g.id=$2"
        ))
        .bind(user)
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(row_to_goal).transpose()
    }
    pub async fn count_goals(&self, user: Uuid) -> Result<i64, sqlx::Error> {
        sqlx::query_scalar("SELECT count(*) FROM pulse_goals WHERE user_id=$1")
            .bind(user)
            .fetch_one(&self.pool)
            .await
    }
    pub async fn insert_goal(
        &self,
        user: Uuid,
        draft: &GoalDraft,
        starts_on: NaiveDate,
    ) -> Result<Uuid, sqlx::Error> {
        let definition = draft
            .definition
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        sqlx::query_scalar(
            "INSERT INTO pulse_goals(user_id,title,kind,direction,period,target,unit,definition,starts_on,deadline) \
             VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING id",
        )
        .bind(user)
        .bind(draft.title.trim())
        .bind(draft.kind.as_str())
        .bind(draft.direction.as_str())
        .bind(draft.period.map(GoalPeriod::as_str))
        .bind(draft.target)
        .bind(draft.unit.trim())
        .bind(definition)
        .bind(starts_on)
        .bind(draft.deadline)
        .fetch_one(&self.pool)
        .await
    }
    pub async fn delete_goal(&self, user: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        Ok(
            sqlx::query("DELETE FROM pulse_goals WHERE user_id=$1 AND id=$2")
                .bind(user)
                .bind(id)
                .execute(&self.pool)
                .await?
                .rows_affected()
                > 0,
        )
    }
    pub async fn add_goal_entry(
        &self,
        user: Uuid,
        goal: Uuid,
        amount: f64,
        note: &str,
        on: NaiveDate,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            "INSERT INTO pulse_goal_entries(goal_id,user_id,amount,note,occurred_on) VALUES($1,$2,$3,$4,$5)",
        )
        .bind(goal)
        .bind(user)
        .bind(amount)
        .bind(note)
        .bind(on)
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}
