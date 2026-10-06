use crate::{
    domain::pulse::*,
    storage::pulse::{ACTION, ACTUAL_SPANS, ALLOWED_SPANS, FLAT_DATA, TIMING},
};
use chrono::{Datelike, Duration, TimeZone, Utc};
use serde_json::json;
use sqlx::PgPool;
use uuid::Uuid;

pub async fn execute(
    pool: &PgPool,
    user: Uuid,
    inputs: &[(PulseDefinition, Measurement)],
) -> Result<Vec<PulseResult>, sqlx::Error> {
    execute_inner(pool, user, inputs, None).await
}

pub async fn execute_cached(
    pool: &PgPool,
    user: Uuid,
    inputs: &[(PulseDefinition, Measurement)],
    key: &str,
    refresh: bool,
    arrived: chrono::DateTime<Utc>,
) -> Result<Vec<PulseResult>, sqlx::Error> {
    execute_inner(pool, user, inputs, Some((key, refresh, arrived))).await
}

async fn execute_inner(
    pool: &PgPool,
    user: Uuid,
    inputs: &[(PulseDefinition, Measurement)],
    cache: Option<(&str, bool, chrono::DateTime<Utc>)>,
) -> Result<Vec<PulseResult>, sqlx::Error> {
    if inputs.is_empty() {
        return Ok(vec![]);
    }
    if inputs.len() > 12 {
        return Err(sqlx::Error::Protocol("Too many charts in one page".into()));
    }
    let now = Utc::now();
    let requests:Vec<_>=inputs.iter().enumerate().map(|(index,(d,m))|{
 let tz:chrono_tz::Tz=d.timezone.parse().expect("validated timezone");
 let local=now.with_timezone(&tz).date_naive()-Duration::days(i64::from(d.offset_days));
 let first=local-Duration::days(i64::from(d.period_days)-1);
 let next=local+Duration::days(1);
 let to=if d.offset_days==0 {now} else {tz.from_local_datetime(&next.and_hms_opt(0,0,0).unwrap()).earliest().unwrap_or_else(||tz.from_utc_datetime(&next.and_hms_opt(0,0,0).unwrap())).with_timezone(&Utc)};
 let from=tz.from_local_datetime(&first.and_hms_opt(0,0,0).unwrap()).earliest().unwrap_or_else(||tz.from_utc_datetime(&first.and_hms_opt(0,0,0).unwrap())).with_timezone(&Utc);
 json!({"index":index,"kind":m.kind,"field":m.field,"source":m.profile.source,"schema_id":m.profile.schema_id,"connection_id":m.profile.connection_id,"category":m.profile.category,"action":m.profile.action,"timing":m.profile.timing,"currency":m.profile.currency,"from":from,"to":to,"bucket":d.bucket,"dimension":d.dimension,"timezone":d.timezone,"scale":m.scale})
 }).collect();
    let sql = format!(
        r#"
 WITH requests AS MATERIALIZED(SELECT value AS r FROM jsonb_array_elements($2::jsonb) WHERE $3::timestamptz IS NOT NULL),
 base AS MATERIALIZED(
 SELECT s.id,s.title,s.schema_id,s.source,s.category,s.start_at,s.end_at,{action} AS action,{timing} AS timing,COALESCE(s.data->>'currency','') AS currency,{flat} AS flat,
 CASE WHEN pg_input_is_valid(s.data->>'observation_end','timestamp with time zone') THEN (s.data->>'observation_end')::timestamptz END AS observed_end,
 CASE WHEN pg_input_is_valid(s.data->>'observation_start','timestamp with time zone') THEN (s.data->>'observation_start')::timestamptz END AS observed_start
 FROM spans s WHERE {allowed} AND {actual}
 AND EXISTS(SELECT 1 FROM requests WHERE r->>'source'=s.source AND r->>'category'=s.category AND COALESCE(r->>'schema_id','')=COALESCE(s.schema_id::text,''))
 AND (s.start_at>=(SELECT min((r->>'from')::timestamptz) FROM requests) OR s.end_at>=(SELECT min((r->>'from')::timestamptz) FROM requests) OR s.start_at IS NULL OR s.source='playstation' OR EXISTS(SELECT 1 FROM requests WHERE r->>'kind'='recurring_cost_projection'))
 ), matched AS MATERIALIZED(
 SELECT r,b.*,COALESCE(b.start_at,b.observed_end) AS at,
 CASE WHEN jsonb_typeof(flat->(r->>'field'))='number' THEN (flat->>(r->>'field'))::numeric END AS num
 FROM base b JOIN requests ON r->>'source'=b.source AND r->>'category'=b.category AND COALESCE(r->>'schema_id','')=COALESCE(b.schema_id::text,'') AND COALESCE(r->>'connection_id','')=COALESCE(b.flat->>'connection_id','') AND r->>'action'=b.action AND r->>'timing'=b.timing AND r->>'currency'=b.currency
 WHERE (r->>'kind'='recurring_cost_projection' AND flat->>'active'='true')
 OR (r->>'kind'<>'recurring_cost_projection' AND ( (b.source='playstation' AND b.timing='first_to_last_played')
 OR (b.timing='observed_counter_delta' AND b.observed_start>=(r->>'from')::timestamptz AND b.observed_end<=(r->>'to')::timestamptz)
 OR (b.timing NOT IN('observed_counter_delta','first_to_last_played') AND COALESCE(b.start_at,b.observed_end)>=(r->>'from')::timestamptz AND COALESCE(b.start_at,b.observed_end)<=(r->>'to')::timestamptz)
 OR (r->>'kind'='known_interval_duration' AND b.end_at>(r->>'from')::timestamptz AND b.start_at<(r->>'to')::timestamptz)))
 ), regular AS (
 SELECT (r->>'index')::int AS index,
 CASE WHEN r->>'bucket' IS NOT NULL THEN to_char(date_trunc(r->>'bucket',at AT TIME ZONE(r->>'timezone')),'YYYY-MM-DD') ELSE COALESCE(NULLIF(CASE WHEN r->>'dimension'='title' THEN title ELSE flat->>(r->>'dimension') END,''),'Unknown') END AS label,
 CASE WHEN r->>'kind'='event_count' THEN 1::numeric
 WHEN r->>'kind'='recurring_cost_projection' THEN num*CASE COALESCE(flat->>'billing_interval',flat->>'interval') WHEN 'month' THEN 1 WHEN 'monthly' THEN 1 WHEN 'year' THEN 1.0/12 WHEN 'yearly' THEN 1.0/12 WHEN 'annual' THEN 1.0/12 WHEN 'week' THEN 52.0/12 WHEN 'weekly' THEN 52.0/12 WHEN 'day' THEN 365.0/12 WHEN 'daily' THEN 365.0/12 WHEN 'quarterly' THEN 1.0/3 END
 ELSE num END * (r->>'scale')::numeric AS val,
 r->>'kind' AS kind, timing,COALESCE(flat->>'title_id',id::text) AS entity
 FROM matched WHERE r->>'kind'<>'known_interval_duration'
 ), intervals AS (
 SELECT (r->>'index')::int AS index,
 CASE WHEN r->>'bucket' IS NOT NULL THEN to_char(t.local_at,'YYYY-MM-DD') ELSE COALESCE(NULLIF(CASE WHEN r->>'dimension'='title' THEN title ELSE flat->>(r->>'dimension') END,''),'Unknown') END AS label,
 EXTRACT(epoch FROM LEAST(end_at,(r->>'to')::timestamptz,(t.local_at+CASE r->>'bucket' WHEN 'week' THEN interval '1 week' WHEN 'month' THEN interval '1 month' ELSE interval '1 day' END) AT TIME ZONE(r->>'timezone'))-GREATEST(start_at,(r->>'from')::timestamptz,t.local_at AT TIME ZONE(r->>'timezone'))) * (r->>'scale')::numeric AS val,
 'known_interval_duration'::text AS kind,timing,id::text AS entity
 FROM matched CROSS JOIN LATERAL generate_series(date_trunc(COALESCE(r->>'bucket','day'),GREATEST(start_at,(r->>'from')::timestamptz) AT TIME ZONE(r->>'timezone')),date_trunc(COALESCE(r->>'bucket','day'),(LEAST(end_at,(r->>'to')::timestamptz)-interval '1 microsecond') AT TIME ZONE(r->>'timezone')),CASE r->>'bucket' WHEN 'week' THEN interval '1 week' WHEN 'month' THEN interval '1 month' ELSE interval '1 day' END)t(local_at)
 WHERE r->>'kind'='known_interval_duration' AND end_at>start_at AND timing NOT IN('observed_counter_delta','first_to_last_played')
 ), values AS (SELECT * FROM regular UNION ALL SELECT * FROM intervals),
 deduped AS (SELECT * FROM values WHERE timing<>'first_to_last_played' UNION ALL SELECT index,label,max(val) AS val,min(kind) AS kind,timing,entity FROM values WHERE timing='first_to_last_played' GROUP BY index,label,timing,entity),
 totals AS (SELECT index,label,CASE WHEN min(kind)='numeric_average' THEN avg(val) ELSE sum(val) END::float8 AS value FROM deduped WHERE val IS NOT NULL GROUP BY index,label),
 ranked AS (SELECT *,row_number() OVER(PARTITION BY index ORDER BY value DESC,label) AS rank FROM totals),
 points AS (SELECT index,jsonb_agg(jsonb_build_object('label',label,'value',value) ORDER BY label) AS points FROM ranked WHERE rank<=CASE WHEN (SELECT r->>'bucket' FROM requests WHERE (r->>'index')::int=ranked.index) IS NULL THEN 20 ELSE 366 END GROUP BY index)
 SELECT COALESCE(jsonb_agg(jsonb_build_object('index',(r->>'index')::int,'points',COALESCE(points.points,'[]')) ORDER BY (r->>'index')::int),'[]') FROM requests LEFT JOIN points ON points.index=(r->>'index')::int
 "#,
        allowed = ALLOWED_SPANS,
        actual = ACTUAL_SPANS,
        flat = FLAT_DATA,
        action = ACTION,
        timing = TIMING
    );
    let (value, computed_at) = if let Some((key, refresh, arrived)) = cache {
        let cached: serde_json::Value = sqlx::query_scalar(
            "SELECT pulse_cached_aggregate($1,$2,(r->>'to')::timestamptz,$4,$5,$6,$7)",
        )
        .bind(user)
        .bind(format!("raw:{key}"))
        .bind(json!(requests))
        .bind(now)
        .bind(&sql)
        .bind(refresh)
        .bind(arrived)
        .fetch_one(pool)
        .await?;
        let computed_at = serde_json::from_value(cached["computed_at"].clone())
            .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
        (cached["rows"].clone(), computed_at)
    } else {
        let value = sqlx::query_scalar(&sql)
            .bind(user)
            .bind(json!(requests))
            .bind(now)
            .fetch_one(pool)
            .await?;
        (value, now)
    };
    let rows = value
        .as_array()
        .ok_or_else(|| sqlx::Error::Protocol("Invalid aggregate result".into()))?;
    inputs
        .iter()
        .enumerate()
        .map(|(i, (d, m))| {
            let mut points: Vec<PulsePoint> = serde_json::from_value(rows[i]["points"].clone())
                .map_err(|e| sqlx::Error::Decode(Box::new(e)))?;
            if let Some(bucket) = &d.bucket {
                let tz: chrono_tz::Tz = d.timezone.parse().unwrap();
                let today =
                    now.with_timezone(&tz).date_naive() - Duration::days(i64::from(d.offset_days));
                let first = today - Duration::days(i64::from(d.period_days) - 1);
                let mut labels = std::collections::BTreeSet::new();
                for offset in 0..d.period_days {
                    let date = first + Duration::days(i64::from(offset));
                    let date = match bucket {
                        Bucket::Day => date,
                        Bucket::Week => {
                            date - Duration::days(i64::from(date.weekday().num_days_from_monday()))
                        }
                        Bucket::Month => date.with_day(1).unwrap(),
                    };
                    labels.insert(date.to_string());
                }
                let recorded: std::collections::BTreeMap<_, _> =
                    points.into_iter().map(|p| (p.label, p.value)).collect();
                points = labels
                    .into_iter()
                    .map(|label| PulsePoint {
                        value: recorded.get(&label).copied().flatten(),
                        label,
                    })
                    .collect();
            }
            Ok(PulseResult {
                source: m.profile.source.clone(),
                points,
                unit: m.unit.clone(),
                quality: m.quality.clone(),
                description: m.description.clone(),
                record_count: m.profile.dated_count,
                undated_count: m.profile.count - m.profile.dated_count,
                computed_at,
                data_as_of: m.profile.last_at,
                error: None,
            })
        })
        .collect()
}
