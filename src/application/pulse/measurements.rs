use crate::domain::pulse::*;
use sha2::{Digest, Sha256};

pub fn definition_hash(definition: &PulseDefinition) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(definition).expect("serializable definition"))
    )
}
#[allow(clippy::too_many_arguments)]
fn measurement(
    p: &SourceProfile,
    kind: MeasurementKind,
    field: Option<&str>,
    title: String,
    unit: &str,
    quality: &str,
    scale: f64,
    description: &str,
) -> Measurement {
    let key = format!("{}:{kind:?}:{field:?}", p.key);
    let is_counter = p.source == "playstation";
    let dimensions: Vec<String> = p.samples.first().and_then(|definition| definition.get("dimensions"))
        .and_then(|value| value.as_array()).into_iter().flatten().filter_map(|value| value.as_str())
        .filter(|field| field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') && p.fields.get(*field).is_some_and(|ty| ty == "string"))
        .take(10).map(str::to_owned).collect();
    let default_dimension = if is_counter && dimensions.iter().any(|d| d == "game") {
        Some("game".into())
    } else {
        None
    };
    Measurement {
        id: format!("{:x}", Sha256::digest(key.as_bytes())),
        profile: p.clone(),
        title,
        description: description.into(),
        kind,
        field: field.map(str::to_owned),
        unit: unit.into(),
        quality: quality.into(),
        scale,
        buckets: if is_counter {
            vec![]
        } else {
            vec![Bucket::Day, Bucket::Week, Bucket::Month]
        },
        dimensions,
        default_dimension,
    }
}
pub fn measurement_catalog(profiles: &[SourceProfile]) -> Vec<Measurement> {
    let mut out = Vec::new();
    for p in profiles {
        let Some(metrics) = p.samples.first().and_then(|v| v.get("metrics")).and_then(|v| v.as_array()) else { continue; };
        for metric in metrics {
            let field = metric.get("field").and_then(|v| v.as_str());
            if let Some(field) = field {
                if field.is_empty() || !field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    || p.fields.get(field).is_none_or(|ty| ty != "number") { continue; }
            }
            if p.timing == "cumulative_lifetime_stat" || p.timing == "observed_counter_delta" { continue; }
            let kind = match metric.get("aggregation").and_then(|v| v.as_str()) {
                Some("count") => MeasurementKind::EventCount,
                Some("sum") if field.is_some() => MeasurementKind::NumericSum,
                Some("avg") if field.is_some() => MeasurementKind::NumericAverage,
                Some("median") if field.is_some() => MeasurementKind::NumericMedian,
                Some("p95") if field.is_some() => MeasurementKind::NumericP95,
                _ => continue,
            };
            let unit = metric.get("unit").and_then(|v| v.as_str()).unwrap_or("events");
            let unit = if unit == "currency" {
                if p.currency.is_empty() { "unknown_currency" } else { &p.currency }
            } else { unit };
            let title = metric.get("title").and_then(|v| v.as_str()).unwrap_or(&p.action);
            out.push(measurement(p, kind, field, format!("{title} ({unit})"), unit, "recorded", 1.0,
                "Computed from recorded events. Missing coverage remains unknown; currencies are never combined."));
        }
    }
    out
}
pub fn validate_definition(
    d: &PulseDefinition,
    catalog: &[Measurement],
) -> Result<Measurement, String> {
    if d.version != 2
        || !(1..=366).contains(&d.period_days)
        || u32::from(d.period_days) + u32::from(d.offset_days) > 366
        || d.top_n.is_some_and(|n| n == 0 || n > 100)
        || d.timezone.parse::<chrono_tz::Tz>().is_err()
    {
        return Err("Choose a valid period and timezone".into());
    }
    let m = catalog
        .iter()
        .find(|m| m.id == d.measurement_id)
        .ok_or("Measurement is unavailable or its source access has changed")?
        .clone();
    if d.chart_type == crate::domain::charts::ChartType::Stat && d.dimension.is_some() {
        return Err("Use a time grouping for a total statistic".into());
    }
    match (&d.bucket, &d.dimension) {
        (Some(bucket), None) if m.buckets.contains(bucket) => {}
        (None, Some(field)) if m.dimensions.contains(field) => {}
        _ => return Err("This grouping is not supported by the recorded measurement".into()),
    }
    if matches!(
        d.chart_type,
        crate::domain::charts::ChartType::Line | crate::domain::charts::ChartType::Area
    ) && d.bucket.is_none()
    {
        return Err("Choose a bar or pie chart for categorical values".into());
    }
    if d.chart_type == crate::domain::charts::ChartType::Pie && !m.profile.currency.is_empty() {
        return Err("Use a bar chart for amounts that may include signed refunds".into());
    }
    Ok(m)
}
