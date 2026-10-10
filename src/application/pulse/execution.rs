use crate::domain::pulse::*;
use chrono::{Duration, Utc};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
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
    if inputs.len() > 12 {
        return Err(sqlx::Error::Protocol("Too many charts".into()));
    }
    if let Some((key, refresh, arrived)) = cache {
        let cached: Option<serde_json::Value> = sqlx::query_scalar("SELECT payload FROM pulse_cache WHERE user_id=$1 AND cache_key=$2 AND expires_at>now() AND (NOT $3 OR created_at >= $4)")
            .bind(user).bind(key).bind(refresh).bind(arrived).fetch_optional(pool).await?;
        if let Some(cached) = cached
            && let Ok(results) = serde_json::from_value(cached)
        {
            return Ok(results);
        }
    }
    let mut results = Vec::with_capacity(inputs.len());
    for (definition, measurement) in inputs {
        results.push(execute_single(pool, user, definition, measurement).await?);
    }
    if let Some((key, _, _)) = cache {
        crate::storage::pulse::PulseRepository::new(pool.clone())
            .put_cache(user, key, json!(results), 60)
            .await?;
    }
    Ok(results)
}

async fn execute_single(
    pool: &PgPool,
    user: Uuid,
    d: &PulseDefinition,
    m: &Measurement,
) -> Result<PulseResult, sqlx::Error> {
    let timezone = d
        .timezone
        .parse::<chrono_tz::Tz>()
        .map_err(|_| sqlx::Error::Protocol("Invalid timezone".into()))?;
    if !(1..=366).contains(&d.period_days) {
        return Err(sqlx::Error::Protocol("Invalid period".into()));
    }
    let now = Utc::now();
    if u32::from(d.period_days) + u32::from(d.offset_days) > 366 {
        return Err(sqlx::Error::Protocol("range exceeds 366 days".into()));
    }
    let end_day =
        now.with_timezone(&timezone).date_naive() - Duration::days(i64::from(d.offset_days));
    let from_day = end_day - Duration::days(i64::from(d.period_days) - 1);
    let end_at = if d.offset_days == 0 {
        now
    } else {
        use chrono::TimeZone;
        timezone
            .from_local_datetime(&(end_day + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap())
            .earliest()
            .ok_or_else(|| sqlx::Error::Protocol("date boundary unavailable in timezone".into()))?
            .with_timezone(&Utc)
    };
    let type_id = m
        .profile
        .schema_id
        .ok_or_else(|| sqlx::Error::Protocol("Missing event type".into()))?;
    let metric_key = format!(
        "{:x}",
        Sha256::digest(
            serde_json::to_vec(&(
                m.id.clone(),
                from_day,
                end_day,
                d.dimension.clone(),
                d.timezone.clone()
            ))
            .unwrap()
        )
    );
    let revision: i64 = sqlx::query_scalar(
        "SELECT coalesce((SELECT data_revision FROM pulse_revisions WHERE user_id=$1),0)",
    )
    .bind(user)
    .fetch_one(pool)
    .await?;
    let distribution = matches!(
        m.kind,
        MeasurementKind::NumericMedian | MeasurementKind::NumericP95
    );
    let mut rows = if !distribution {
        sqlx::query("SELECT day, dimension_value AS dimension, aggregate_value::float8 AS sum_value, count_events::bigint AS cnt, (metadata->>'valid_count')::bigint AS valid_count FROM pulse_daily_aggregates WHERE user_id=$1 AND metric_key=$2 AND data_revision=$3 ORDER BY day")
            .bind(user).bind(&metric_key).bind(revision).fetch_all(pool).await?
    } else {
        Vec::new()
    };
    if rows.is_empty() {
        let field = m.field.as_deref().unwrap_or("");
        if !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(sqlx::Error::Protocol("Invalid metric field".into()));
        }
        let dimension = d.dimension.as_deref().unwrap_or("");
        if !dimension
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(sqlx::Error::Protocol("Invalid dimension".into()));
        }
        let spending = m.profile.action == "transaction" && field == "amount";
        let value = if spending {
            "CASE WHEN et.value='refund' AND jsonb_typeof(te.content->'amount')='number' THEN -abs((te.content->>'amount')::numeric) WHEN et.value='transaction' AND te.content->>'is_spending'='true' AND jsonb_typeof(te.content->'amount')='number' THEN abs((te.content->>'amount')::numeric) ELSE NULL END".to_owned()
        } else if m.kind == MeasurementKind::EventCount {
            "1::numeric".to_owned()
        } else if m.kind == MeasurementKind::KnownIntervalDuration {
            "CASE WHEN te.content->>'timing' IN ('cumulative_lifetime_stat','observed_counter_delta') THEN NULL WHEN te.ended_at>te.occurred_at THEN extract(epoch FROM te.ended_at-te.occurred_at) WHEN jsonb_typeof(te.content->'duration_seconds')='number' THEN (te.content->>'duration_seconds')::numeric ELSE NULL END".to_owned()
        } else {
            format!(
                "CASE WHEN jsonb_typeof(te.content->'{field}')='number' THEN (te.content->>'{field}')::numeric ELSE NULL END"
            )
        };
        let percentile = if m.kind == MeasurementKind::NumericP95 {
            "0.95"
        } else {
            "0.5"
        };
        let distribution_value = if distribution {
            format!(
                ", percentile_cont({percentile}) WITHIN GROUP (ORDER BY val::float8) AS distribution_value"
            )
        } else {
            String::new()
        };
        let day_expression = if distribution {
            if d.dimension.is_some() || d.chart_type == crate::domain::charts::ChartType::Stat {
                "$3::date".to_owned()
            } else {
                let bucket = match d.bucket {
                    Some(Bucket::Week) => "week",
                    Some(Bucket::Month) => "month",
                    _ => "day",
                };
                format!("date_trunc('{bucket}', te.occurred_at AT TIME ZONE $2)::date")
            }
        } else {
            "(te.occurred_at AT TIME ZONE $2)::date".to_owned()
        };
        let sql = format!(
            r#"
            WITH event_values AS (
                SELECT {day_expression} AS day,
                    CASE WHEN $7='' THEN '' ELSE coalesce(nullif(te.content->>$7,''),'unknown') END AS dimension,
                    {value} AS val
                FROM timeline_events te JOIN timeline_event_types et ON et.id=te.event_type_id
                WHERE te.user_id=$1 AND te.record_state='active'
                    AND te.occurred_at >= ($3::date::timestamp AT TIME ZONE $2) AND te.occurred_at <= $4
                    AND (($8 AND et.value IN ('transaction','refund')) OR (NOT $8 AND te.event_type_id=$5))
                    AND coalesce(te.content->>'currency','')=$6
                    AND coalesce(te.content->>'timing','')=$9
                    AND coalesce(te.content->>'timing','') NOT IN ('cumulative_lifetime_stat','observed_counter_delta')
            ) SELECT day, dimension, sum(val)::float8 AS sum_value, count(*)::bigint AS cnt,
                count(val)::bigint AS valid_count {distribution_value}
            FROM event_values GROUP BY day, dimension ORDER BY day
        "#
        );
        let mut read_tx = pool.begin().await?;
        sqlx::query("SET LOCAL statement_timeout='5s'")
            .execute(&mut *read_tx)
            .await?;
        rows = sqlx::query(&sql)
            .bind(user)
            .bind(&d.timezone)
            .bind(from_day)
            .bind(end_at)
            .bind(type_id)
            .bind(&m.profile.currency)
            .bind(dimension)
            .bind(spending)
            .bind(&m.profile.timing)
            .fetch_all(&mut *read_tx)
            .await?;
        read_tx.commit().await?;
        if !distribution {
            let mut tx = pool.begin().await?;
            sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
                .bind(format!("rollup:{user}:{metric_key}"))
                .execute(&mut *tx)
                .await?;
            let current: i64 = sqlx::query_scalar(
                "SELECT coalesce((SELECT data_revision FROM pulse_revisions WHERE user_id=$1),0)",
            )
            .bind(user)
            .fetch_one(&mut *tx)
            .await?;
            if current == revision {
                sqlx::query(
                    "DELETE FROM pulse_daily_aggregates WHERE user_id=$1 AND metric_key=$2",
                )
                .bind(user)
                .bind(&metric_key)
                .execute(&mut *tx)
                .await?;
                for row in &rows {
                    sqlx::query("INSERT INTO pulse_daily_aggregates (user_id, group_id, event_type_id, day, metric_key, aggregate_value, count_events, metadata, timezone, currency, dimension_value, data_revision) SELECT $1, group_id, id, $3, $4, $5, $6, $7, $8, $9, $10, $11 FROM timeline_event_types WHERE id=$2")
                        .bind(user).bind(type_id).bind(row.get::<chrono::NaiveDate,_>("day")).bind(&metric_key)
                        .bind(row.get::<Option<f64>,_>("sum_value").unwrap_or(0.0)).bind(row.get::<i64,_>("cnt"))
                        .bind(json!({"valid_count": row.get::<i64,_>("valid_count")})).bind(&d.timezone).bind(&m.profile.currency)
                        .bind(row.get::<String,_>("dimension")).bind(revision).execute(&mut *tx).await?;
                }
                tx.commit().await?;
            }
        }
    }
    let mut buckets = std::collections::BTreeMap::<String, (f64, i64, i64)>::new();
    for row in &rows {
        let day: chrono::NaiveDate = row.get("day");
        let label = if d.chart_type == crate::domain::charts::ChartType::Stat {
            "Total".to_owned()
        } else if d.dimension.is_some() {
            row.get::<String, _>("dimension")
        } else {
            use chrono::Datelike;
            match d.bucket {
                Some(Bucket::Week) => (day
                    - Duration::days(i64::from(day.weekday().num_days_from_monday())))
                .to_string(),
                Some(Bucket::Month) => day.with_day(1).unwrap().to_string(),
                _ => day.to_string(),
            }
        };
        let entry = buckets.entry(label).or_default();
        entry.0 += row
            .get::<Option<f64>, _>(if distribution {
                "distribution_value"
            } else {
                "sum_value"
            })
            .unwrap_or(0.0);
        entry.1 += row.get::<i64, _>("valid_count");
        entry.2 += row.get::<i64, _>("cnt");
    }
    let record_count = buckets.values().map(|v| v.2).sum();
    let valid_count: i64 = buckets.values().map(|v| v.1).sum();
    let total_sum: f64 = buckets.values().map(|v| v.0).sum();
    let total = if valid_count == 0
        || (distribution && d.chart_type != crate::domain::charts::ChartType::Stat)
    {
        None
    } else {
        Some(if m.kind == MeasurementKind::NumericAverage {
            total_sum / valid_count as f64 * m.scale
        } else {
            total_sum * m.scale
        })
    };
    let mut points: Vec<PulsePoint> = buckets
        .into_iter()
        .map(|(label, (sum, count, _))| PulsePoint {
            label,
            value: if count == 0 {
                None
            } else {
                Some(if m.kind == MeasurementKind::NumericAverage {
                    sum / count as f64 * m.scale
                } else {
                    sum * m.scale
                })
            },
        })
        .collect();
    if d.dimension.is_some() {
        points.sort_by(|left, right| {
            right
                .value
                .unwrap_or(f64::NEG_INFINITY)
                .total_cmp(&left.value.unwrap_or(f64::NEG_INFINITY))
                .then_with(|| left.label.cmp(&right.label))
        });
        points.truncate(usize::from(d.top_n.unwrap_or(20)));
    }
    let source_coverage = sqlx::query("SELECT connector_id,coverage_start,coverage_end,sync_mode,is_healthy,last_checked_at,metadata FROM connector_coverage WHERE user_id=$1 ORDER BY connector_id")
        .bind(user).fetch_all(pool).await?;
    let coverage = json!({"state":"partial","scope":"recorded_observations","range_start":from_day,"range_end":end_day,"timezone":d.timezone,
        "observed_records":record_count,"values_missing":record_count-valid_count,"empty_buckets":"unknown","sources":source_coverage.into_iter().map(|row| json!({
            "connector":row.get::<String,_>("connector_id"),"start":row.get::<Option<chrono::DateTime<Utc>>,_>("coverage_start"),
            "end":row.get::<Option<chrono::DateTime<Utc>>,_>("coverage_end"),"mode":row.get::<String,_>("sync_mode"),"healthy":row.get::<bool,_>("is_healthy"),
            "last_checked_at":row.get::<Option<chrono::DateTime<Utc>>,_>("last_checked_at")})).collect::<Vec<_>>()});
    Ok(PulseResult {
        coverage,
        total,
        source: m.profile.source.clone(),
        points,
        unit: m.unit.clone(),
        quality: m.quality.clone(),
        description: m.description.clone(),
        record_count,
        undated_count: 0,
        computed_at: Utc::now(),
        data_as_of: m.profile.last_at.filter(|at| *at <= end_at),
        error: None,
    })
}
