use crate::domain::pulse::*;
use serde::{Deserialize, Serialize};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub(crate) const NATIVE_CONTEXT: &str = r#"(SELECT CASE WHEN count(*)=1 THEN min(id::text)::uuid END FROM (SELECT u.id FROM user_contexts u JOIN platform_deployments d ON d.id=u.deployment_id JOIN host_apps h ON h.id=u.host_app_id AND h.deployment_id=d.id WHERE u.user_id=$1 AND u.host_user_id IN ($1::text,'vox-account:'||$1::text) AND u.organization_id IS NULL AND d.external_key='vox.standalone.deployment' AND h.external_key='vox.standalone.web') native)"#;

pub const ALLOWED_SPANS: &str = r#"s.user_id=$1 AND s.user_context_id=(SELECT CASE WHEN count(*)=1 THEN min(id::text)::uuid END FROM (SELECT u.id FROM user_contexts u JOIN platform_deployments d ON d.id=u.deployment_id JOIN host_apps h ON h.id=u.host_app_id AND h.deployment_id=d.id WHERE u.user_id=$1 AND u.host_user_id IN ($1::text,'vox-account:'||$1::text) AND u.organization_id IS NULL AND d.external_key='vox.standalone.deployment' AND h.external_key='vox.standalone.web') native) AND (s.schema_id IS NULL OR EXISTS(SELECT 1 FROM data_schemas ds WHERE ds.id=s.schema_id AND ds.state='active' AND (ds.user_id=$1 OR ds.user_id IS NULL))) AND (NOT(s.data ? 'connection_id') OR EXISTS(SELECT 1 FROM vox_connections c WHERE c.id::text=s.data->>'connection_id' AND c.user_id=$1 AND c.user_context_id=s.user_context_id AND c.authorization_state='authorized' AND c.assistant_read AND c.sync_timeline AND c.consented_at IS NOT NULL) OR EXISTS(SELECT 1 FROM external_connections x WHERE x.id::text=s.data->>'connection_id' AND x.user_context_id=s.user_context_id AND x.authorization_state='authorized' AND (x.expires_at IS NULL OR x.expires_at>now()) AND NOT EXISTS(SELECT 1 FROM vox_connections c WHERE c.id=x.id)))"#;
pub const ACTUAL_SPANS: &str = r#"s.status='done' AND COALESCE(s.data->>'direction','')<>'due' AND COALESCE(s.data->>'duplicate_of','')=''"#;
pub const FLAT_DATA: &str = r#"s.data || COALESCE((SELECT jsonb_object_agg('provider_data.'||e.key,e.value) FROM jsonb_each(CASE WHEN jsonb_typeof(s.data->'provider_data')='object' THEN s.data->'provider_data' ELSE '{}'::jsonb END)e),'{}'::jsonb) || jsonb_strip_nulls(jsonb_build_object('artist',s.data#>'{provider_data,artists,0,name}'))"#;
pub const ACTION: &str = "COALESCE(s.data#>>'{provider_data,action}',s.data->>'direction','')";
pub const TIMING: &str = "COALESCE(s.data->>'timing','')";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Inventory {
    pub profiles: Vec<SourceProfile>,
    pub source_count: usize,
    pub record_count: i64,
}
#[derive(Clone, Debug)]
pub struct PulseMetadata {
    pub revision: String,
    pub connections: Vec<PulseConnection>,
    pub charts: Vec<SavedPulseChart>,
    pub dismissed: Vec<String>,
    pub saved_hashes: Vec<String>,
    pub caches: serde_json::Value,
    pub legacy_boards: Vec<serde_json::Value>,
    pub next_cursor: Option<Uuid>,
}
#[derive(Clone)]
pub struct PulseRepository {
    pub pool: PgPool,
}
impl PulseRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
    pub async fn metadata(
        &self,
        user: Uuid,
        cursor: Option<Uuid>,
    ) -> Result<PulseMetadata, sqlx::Error> {
        let row=sqlx::query(r#"
 SELECT concat(COALESCE(r.data_revision,0),':',COALESCE(r.discovery_revision,0),':',g.revision,':scoped-v1') AS revision,
 COALESCE((SELECT jsonb_agg(jsonb_build_object('connector_id',c.connector_id,'last_synced_at',c.last_synced_at,'authorization_state',c.authorization_state,'sync_timeline',c.sync_timeline,'assistant_read',c.assistant_read)) FROM vox_connections c WHERE c.user_id=$1 AND c.user_context_id=(SELECT CASE WHEN count(*)=1 THEN min(id::text)::uuid END FROM (SELECT u.id FROM user_contexts u JOIN platform_deployments d ON d.id=u.deployment_id JOIN host_apps h ON h.id=u.host_app_id AND h.deployment_id=d.id WHERE u.user_id=$1 AND u.host_user_id IN ($1::text,'vox-account:'||$1::text) AND u.organization_id IS NULL AND d.external_key='vox.standalone.deployment' AND h.external_key='vox.standalone.web') native)),'[]') AS connections,
 COALESCE((SELECT jsonb_agg(to_jsonb(q)) FROM (SELECT id,title,definition,created_at FROM pulse_saved_charts WHERE user_id=$1 AND ($2::uuid IS NULL OR id>$2) ORDER BY id LIMIT 13)q),'[]') AS charts,
 COALESCE((SELECT jsonb_agg(definition_hash) FROM pulse_saved_charts WHERE user_id=$1),'[]') AS saved_hashes,
 COALESCE((SELECT jsonb_agg(definition_hash) FROM pulse_dismissals WHERE user_id=$1),'[]') AS dismissed,
 COALESCE((SELECT jsonb_object_agg(cache_key,payload) FROM pulse_cache WHERE user_id=$1 AND expires_at>now()),'{}') AS caches,
 COALESCE((SELECT jsonb_agg(to_jsonb(q)) FROM (SELECT b.id,b.name,b.created_at,count(c.id) AS chart_count FROM chart_boards b LEFT JOIN charts c ON c.board_id=b.id WHERE b.user_id=$1 GROUP BY b.id ORDER BY b.created_at DESC LIMIT 100)q),'[]') AS legacy_boards
 FROM pulse_global_revision g LEFT JOIN pulse_revisions r ON r.user_id=$1 WHERE g.id
 "#).bind(user).bind(cursor).fetch_one(&self.pool).await?;
        let mut charts: Vec<SavedPulseChart> = serde_json::from_value(row.get("charts"))
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        let next_cursor = if charts.len() > 12 {
            charts.truncate(12);
            charts.last().map(|c| c.id)
        } else {
            None
        };
        fn decode<T: serde::de::DeserializeOwned>(
            row: &sqlx::postgres::PgRow,
            name: &str,
        ) -> Result<T, sqlx::Error> {
            serde_json::from_value(row.get(name)).map_err(|e| sqlx::Error::Decode(Box::new(e)))
        }
        Ok(PulseMetadata {
            revision: row.get("revision"),
            connections: decode(&row, "connections")?,
            charts,
            dismissed: decode(&row, "dismissed")?,
            saved_hashes: decode(&row, "saved_hashes")?,
            caches: row.get("caches"),
            legacy_boards: decode(&row, "legacy_boards")?,
            next_cursor,
        })
    }
    pub async fn profiles(&self, user: Uuid) -> Result<Vec<SourceProfile>, sqlx::Error> {
        Ok(self.inventory(user).await?.profiles)
    }
    pub async fn inventory(&self, user: Uuid) -> Result<Inventory, sqlx::Error> {
        let query = format!(
            r#"
 WITH raw AS MATERIALIZED (
 SELECT s.schema_id,s.data->>'connection_id' AS connection_id,s.source,s.category,{action} AS action,{timing} AS timing,
 COALESCE(s.data->>'currency','') AS currency,s.start_at,s.end_at,{flat} AS flat
 FROM spans s WHERE {allowed} AND {actual}
 ), keyed AS MATERIALIZED (
 SELECT *,md5(jsonb_build_array(schema_id,connection_id,source,category,action,timing,currency)::text) AS key FROM raw
 ), summary AS (
 SELECT key,schema_id,connection_id,source,category,action,timing,currency,count(*) AS count,count(start_at) AS dated_count,min(start_at) AS first_at,max(start_at) AS last_at,
 count(*) FILTER(WHERE end_at>start_at AND timing NOT IN('first_to_last_played','observed_history_range') AND timing<>'observed_counter_delta') AS known_intervals
 FROM keyed GROUP BY key,schema_id,connection_id,source,category,action,timing,currency
 ), recent_fields AS (
 SELECT keyed.key,e.key AS field,CASE WHEN count(DISTINCT jsonb_typeof(e.value))=1 THEN min(jsonb_typeof(e.value)) ELSE 'mixed' END AS kind
 FROM keyed CROSS JOIN LATERAL jsonb_each(flat)e
 WHERE (start_at>=now()-interval '90 days' OR start_at IS NULL OR source='playstation') AND jsonb_typeof(e.value) IN('string','number','boolean') AND length(e.key)<=80
 AND e.key NOT IN('connection_id','account_hint','reference','fingerprint','source_id')
 GROUP BY keyed.key,e.key
 ), field_summary AS (SELECT key,jsonb_object_agg(field,kind) AS fields FROM recent_fields GROUP BY key),
 schema_hints AS (
 SELECT sc.schema_id,jsonb_object_agg(p.key,jsonb_strip_nulls(jsonb_build_object('unit',COALESCE(p.value->>'x-unit',p.value->>'unit'),'aggregation',p.value->>'x-aggregation','label',COALESCE(p.value->>'x-label',p.value->>'title'),'ignore',CASE WHEN p.value->>'x-measure'='false' THEN true END))) AS hints
 FROM (SELECT DISTINCT schema_id FROM summary WHERE schema_id IS NOT NULL) sc
 JOIN data_schemas ds ON ds.id=sc.schema_id
 CROSS JOIN LATERAL jsonb_each(CASE WHEN jsonb_typeof(ds.json_schema->'properties')='object' THEN ds.json_schema->'properties' ELSE '{{}}'::jsonb END) p
 WHERE jsonb_typeof(p.value)='object' GROUP BY sc.schema_id),
 selected AS (SELECT summary.*,COALESCE(f.fields,'{{}}'::jsonb) AS fields,COALESCE(h.hints,'{{}}'::jsonb) AS field_hints,'[]'::jsonb AS samples FROM summary LEFT JOIN field_summary f USING(key) LEFT JOIN schema_hints h ON h.schema_id=summary.schema_id ORDER BY count DESC,key)
 SELECT jsonb_build_object('profiles',COALESCE((SELECT jsonb_agg(to_jsonb(selected)) FROM selected),'[]'), 'source_count',(SELECT count(*) FROM summary),'record_count',COALESCE((SELECT sum(count) FROM summary),0)) AS inventory
 "#,
            action = ACTION,
            timing = TIMING,
            flat = FLAT_DATA,
            allowed = ALLOWED_SPANS,
            actual = ACTUAL_SPANS
        );
        let value: serde_json::Value = sqlx::query_scalar(&query)
            .bind(user)
            .fetch_one(&self.pool)
            .await?;
        serde_json::from_value(value).map_err(|e| sqlx::Error::Decode(Box::new(e)))
    }
    pub async fn put_cache(
        &self,
        user: Uuid,
        key: &str,
        payload: serde_json::Value,
        seconds: i32,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(r#"WITH pruned AS (DELETE FROM pulse_cache WHERE user_id=$1 AND cache_key IN(SELECT cache_key FROM pulse_cache WHERE user_id=$1 ORDER BY expires_at DESC OFFSET 63))
 INSERT INTO pulse_cache(user_id,cache_key,payload,expires_at) VALUES($1,$2,$3,now()+$4*interval '1 second') ON CONFLICT(user_id,cache_key) DO UPDATE SET payload=EXCLUDED.payload,expires_at=EXCLUDED.expires_at,created_at=now()"#).bind(user).bind(key).bind(payload).bind(seconds).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn delete_chart(&self, user: Uuid, id: Uuid) -> Result<bool, sqlx::Error> {
        let done = sqlx::query("DELETE FROM pulse_saved_charts WHERE user_id=$1 AND id=$2")
            .bind(user)
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(done.rows_affected() > 0)
    }
    pub async fn dismiss(&self, user: Uuid, hash: &str) -> Result<(), sqlx::Error> {
        sqlx::query("INSERT INTO pulse_dismissals(user_id,definition_hash) VALUES($1,$2) ON CONFLICT DO NOTHING").bind(user).bind(hash).execute(&self.pool).await?;
        Ok(())
    }
    pub async fn save(
        &self,
        user: Uuid,
        input: &SavePulseInput,
        revision: &str,
        hash: &str,
    ) -> Result<SavedPulseChart, sqlx::Error> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("INSERT INTO pulse_revisions(user_id) VALUES($1) ON CONFLICT DO NOTHING")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        sqlx::query("SELECT user_id FROM pulse_revisions WHERE user_id=$1 FOR UPDATE")
            .bind(user)
            .execute(&mut *tx)
            .await?;
        let current:String=sqlx::query_scalar("SELECT concat(r.data_revision,':',r.discovery_revision,':',g.revision,':scoped-v1') FROM pulse_revisions r CROSS JOIN pulse_global_revision g WHERE r.user_id=$1 AND g.id FOR SHARE OF g").bind(user).fetch_one(&mut *tx).await?;
        if current != revision {
            return Err(sqlx::Error::Protocol(
                "Source data changed; preview again before saving".into(),
            ));
        }
        let existing=sqlx::query("SELECT id,title,definition,created_at FROM pulse_saved_charts WHERE user_id=$1 AND idempotency_key=$2").bind(user).bind(input.idempotency_key).fetch_optional(&mut *tx).await?;
        let definition = serde_json::to_value(&input.definition)
            .map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
        let row = if let Some(row) = existing {
            if row.get::<serde_json::Value, _>("definition") != definition
                || row.get::<String, _>("title") != input.title.trim()
            {
                return Err(sqlx::Error::Protocol(
                    "Idempotency key was already used for another chart".into(),
                ));
            }
            row
        } else {
            sqlx::query("INSERT INTO pulse_saved_charts(user_id,idempotency_key,title,definition,definition_hash) VALUES($1,$2,$3,$4,$5) RETURNING id,title,definition,created_at").bind(user).bind(input.idempotency_key).bind(input.title.trim()).bind(definition).bind(hash).fetch_one(&mut *tx).await?
        };
        let chart = SavedPulseChart {
            id: row.get("id"),
            title: row.get("title"),
            definition: serde_json::from_value(row.get("definition"))
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?,
            created_at: row.get("created_at"),
            result: None,
        };
        tx.commit().await?;
        Ok(chart)
    }
}
