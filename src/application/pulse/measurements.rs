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
    let dimensions: Vec<String> = p
        .fields
        .iter()
        .filter(|(k, v)| {
            *v == "string"
                && !k.ends_with("id")
                && !k.contains("connection")
                && !k.contains("observation")
                && !k.contains("timing")
                && !k.contains("url")
                && !k.contains("date")
                && !k.contains("time")
                && !k.contains("source")
                && !k.contains("currency")
        })
        .take(10)
        .map(|(k, _)| k.clone())
        .collect();
    let mut dimensions = dimensions;
    if matches!(
        p.source.as_str(),
        "google_maps" | "google_calendar" | "swiggy" | "zomato"
    ) && !dimensions.iter().any(|d| d == "title")
    {
        dimensions.push("title".into());
    }
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
        buckets: if is_counter
            && !matches!(
                p.timing.as_str(),
                "observed_counter_delta" | "estimated_from_totals"
            ) {
            vec![]
        } else if is_counter {
            // Counter deltas and estimated sessions have no trustworthy day.
            vec![Bucket::Week, Bucket::Month]
        } else {
            vec![Bucket::Day, Bucket::Week, Bucket::Month]
        },
        dimensions,
        default_dimension,
    }
}
struct NumericRule {
    financial: bool,
    kind: MeasurementKind,
    unit: String,
    scale: f64,
    label: Option<String>,
}
fn name_tokens(field: &str) -> Vec<String> {
    field
        .rsplit('.')
        .next()
        .unwrap_or(field)
        .to_lowercase()
        .split(['_', '-', ' '])
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect()
}
/// Decides whether a numeric field is a sensible measurement, and how to sum it and label its unit.
/// Schema hints win over name heuristics; ids, coordinates and cumulative counters are skipped.
fn numeric_rule(field: &str, ty: &str, p: &SourceProfile) -> Option<NumericRule> {
    if ty != "number" {
        return None;
    }
    let hint = p.field_hints.get(field);
    if hint.is_some_and(|h| h.ignore) {
        return None;
    }
    let tokens = name_tokens(field);
    let has = |words: &[&str]| tokens.iter().any(|t| words.contains(&t.as_str()));
    let financial = has(&[
        "amount", "cost", "price", "spent", "spend", "fee", "fees", "tip",
    ]);
    const SKIP: &[&str] = &[
        "id",
        "ids",
        "index",
        "idx",
        "version",
        "epoch",
        "timestamp",
        "ts",
        "offset",
        "lat",
        "lng",
        "lon",
        "latitude",
        "longitude",
        "year",
        "month",
        "day",
        "zip",
        "pin",
        "pincode",
        "phone",
        "port",
        "eta",
        "lifetime",
        "cumulative",
        "rank",
        "position",
        "seq",
        "sequence",
        "page",
    ];
    let declared = hint.is_some_and(|h| h.aggregation.is_some() || h.unit.is_some());
    if !declared && (has(SKIP) || (has(&["total"]) && !financial)) {
        return None;
    }
    let averaged = match hint.and_then(|h| h.aggregation.as_deref()) {
        Some("avg" | "average" | "mean") => true,
        Some(_) => false,
        None => has(&[
            "weight",
            "score",
            "rating",
            "rate",
            "ratio",
            "percent",
            "pct",
            "temperature",
            "temp",
            "bpm",
            "pace",
            "speed",
            "avg",
            "average",
            "level",
            "mood",
            "glucose",
        ]),
    };
    let last = tokens.last().map(String::as_str).unwrap_or("");
    let (unit, scale) = if let Some(unit) = hint.and_then(|h| h.unit.clone()) {
        (unit, 1.0)
    } else {
        match last {
            "seconds" | "secs" | "sec" => ("hours".to_string(), 1.0 / 3600.0),
            "minutes" | "mins" | "min" => ("hours".to_string(), 1.0 / 60.0),
            "ms" | "millis" | "milliseconds" => ("hours".to_string(), 1.0 / 3_600_000.0),
            "hours" | "hrs" | "hr" => ("hours".to_string(), 1.0),
            "km" | "kg" | "mi" | "miles" | "kcal" | "bpm" | "cm" | "kb" | "mb" | "gb" => {
                (last.to_string(), 1.0)
            }
            _ => (tokens.join(" "), 1.0),
        }
    };
    Some(NumericRule {
        financial,
        kind: if averaged {
            MeasurementKind::NumericAverage
        } else {
            MeasurementKind::NumericSum
        },
        unit,
        scale,
        label: hint.and_then(|h| h.label.clone()),
    })
}
pub fn measurement_catalog(profiles: &[SourceProfile]) -> Vec<Measurement> {
    let mut out = vec![];
    for p in profiles {
        if p.count == 0 {
            continue;
        }
        let title = match (p.source.as_str(), p.action.as_str()) {
            ("spotify", _) => "Recorded Spotify plays".to_string(),
            ("youtube", "watch") => "Watched YouTube videos".to_string(),
            ("youtube", "playlist_addition") => "YouTube playlist additions".to_string(),
            ("youtube", "like") => "YouTube likes".to_string(),
            ("playstation", _) => "Recorded gaming updates".to_string(),
            ("google_maps", _) => "Places visited".to_string(),
            ("google_calendar", _) => "Calendar events".to_string(),
            ("swiggy" | "zomato", _) => "Food orders".to_string(),
            _ => format!("{} entries", p.category.replace('_', " ")),
        };
        if p.dated_count > 0 && p.source != "playstation" {
            out.push(measurement(
                p,
                MeasurementKind::EventCount,
                None,
                title,
                "events",
                "recorded",
                1.0,
                "Counts recorded completed events. Missing capture periods remain gaps.",
            ));
        }
        if p.source == "spotify"
            && p.fields
                .get("provider_data.reported_track_duration_ms")
                .is_some_and(|v| v == "number")
        {
            out.push(measurement(p,MeasurementKind::NumericSum,Some("provider_data.reported_track_duration_ms"),"Estimated Spotify listening hours".into(),"hours","estimated",1.0/3_600_000.0,"Adds full track lengths for recorded plays. Skips and partial playback are unknown; this is not measured listening time."));
        } else if p.source == "playstation" && p.timing == "estimated_from_totals" {
            if p.known_intervals > 0 {
                out.push(measurement(
                    p,
                    MeasurementKind::KnownIntervalDuration,
                    None,
                    "Estimated gameplay hours".into(),
                    "hours",
                    "estimated",
                    1.0 / 3600.0,
                    "Sessions estimated from PlayStation lifetime totals and spread between first and last played. PlayStation does not report real session days, so weekly shape is approximate.",
                ));
            }
        } else if p.source == "playstation" {
            let (field, title, description) = if p.timing == "observed_counter_delta" {
                (
                    "duration_seconds",
                    "Recorded gameplay increases",
                    "Playtime increases seen between syncs, grouped by when each increase was observed. Exact session days are unknown, and history before capture began is unavailable.",
                )
            } else {
                (
                    "total_seconds",
                    "Lifetime gameplay by game",
                    "Provider lifetime totals by game. These are not sessions or daily playtime.",
                )
            };
            if p.fields.get(field).is_some_and(|v| v == "number") {
                out.push(measurement(
                    p,
                    MeasurementKind::NumericSum,
                    Some(field),
                    title.into(),
                    "hours",
                    "provider_total",
                    1.0 / 3600.0,
                    description,
                ));
            }
        } else if p.source != "youtube" && p.source != "spotify" {
            let is_subscription = p.category.to_lowercase().contains("subscription");
            for (field, ty) in &p.fields {
                let Some(rule) = numeric_rule(field, ty, p) else {
                    continue;
                };
                let financial = rule.financial;
                if financial && p.currency.is_empty() {
                    continue;
                }
                if is_subscription && financial {
                    if p.fields.contains_key("billing_interval")
                        || p.fields.contains_key("interval")
                    {
                        let mut m = measurement(
                            p,
                            MeasurementKind::RecurringCostProjection,
                            Some(field),
                            format!("Monthly subscription cost ({})", p.currency),
                            &p.currency,
                            "projected",
                            1.0,
                            "Normalizes explicit active subscriptions to monthly cost. This is a projection, not actual payments.",
                        );
                        m.buckets.clear();
                        m.default_dimension = Some("title".into());
                        m.dimensions.push("title".into());
                        out.push(m);
                    }
                    continue;
                }
                if p.dated_count == 0 {
                    continue;
                }
                let kind = rule.kind;
                let unit = if financial {
                    p.currency.as_str()
                } else {
                    rule.unit.as_str()
                };
                let name = if financial {
                    format!(
                        "{} {} ({})",
                        p.category.replace('_', " "),
                        if matches!(p.action.as_str(), "credit" | "refund" | "income") {
                            p.action.as_str()
                        } else {
                            "spending"
                        },
                        p.currency
                    )
                } else {
                    rule.label.clone().unwrap_or_else(|| {
                        let mut words = name_tokens(field);
                        let timed = matches!(
                            words.last().map(String::as_str),
                            Some(
                                "seconds"
                                    | "secs"
                                    | "sec"
                                    | "minutes"
                                    | "mins"
                                    | "min"
                                    | "ms"
                                    | "millis"
                                    | "milliseconds"
                            )
                        );
                        if rule.unit == "hours" && timed {
                            *words.last_mut().unwrap() = "hours".into();
                        }
                        format!("{} {}", p.category.replace('_', " "), words.join(" "))
                    })
                };
                let scale = if financial { 1.0 } else { rule.scale };
                out.push(measurement(p,kind,Some(field),name,unit,"recorded",scale,"Uses recorded numeric values and event dates. Currency groups are kept separate; refunds keep their signed values."));
            }
            if p.known_intervals > 0 && !is_subscription {
                out.push(measurement(
                    p,
                    MeasurementKind::KnownIntervalDuration,
                    None,
                    format!("{}{} hours", if p.source == "google_calendar" { "Scheduled " } else { "" }, p.category.replace('_', " ")),
                    "hours",
                    if p.source == "google_calendar" { "scheduled" } else { "measured" },
                    1.0 / 3600.0,
                    if p.source == "google_calendar" { "Splits recorded calendar intervals across local day boundaries. Scheduled time does not establish attendance." } else { "Splits explicitly known completed intervals across your local day boundaries." },
                ));
            }
        }
    }
    out.sort_by_key(|m| std::cmp::Reverse(m.profile.count));
    out
}
pub fn validate_definition(
    d: &PulseDefinition,
    catalog: &[Measurement],
) -> Result<Measurement, String> {
    if d.version != 2
        || !(1..=3650).contains(&d.period_days)
        || d.offset_days > 730
        || d.timezone.parse::<chrono_tz::Tz>().is_err()
    {
        return Err("Choose a valid period and timezone".into());
    }
    let m = catalog
        .iter()
        .find(|m| m.id == d.measurement_id)
        .ok_or("Measurement is unavailable or its source access has changed")?
        .clone();
    if d.period_days > 365 && d.bucket == Some(Bucket::Day) {
        return Err("Use weeks or months for periods longer than a year".into());
    }
    if d.top_n.is_some_and(|n| !(1..=20).contains(&n)) || (d.top_n.is_some() && d.bucket.is_some())
    {
        return Err("Top-N applies to category charts, from 1 to 20".into());
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
