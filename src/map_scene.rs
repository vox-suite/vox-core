use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use uuid::Uuid;

pub const MAX_ITEMS: usize = 50;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MapScene {
    #[serde(default)]
    pub rev: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<Camera>,
    #[serde(default)]
    pub pins: Vec<Pin>,
    #[serde(default)]
    pub arcs: Vec<Arc>,
    #[serde(default)]
    pub columns: Vec<Column>,
    #[serde(default)]
    pub highlights: Vec<Highlight>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub narration_hint: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Camera {
    pub lng: f64,
    pub lat: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zoom: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pitch: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bearing: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u32>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PinKind {
    Place,
    Task,
    Spend,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Pin {
    pub id: String,
    pub lng: f64,
    pub lat: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub kind: PinKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<Uuid>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Arc {
    pub id: String,
    pub from: [f64; 2],
    pub to: [f64; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default)]
    pub delay_ms: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Column {
    pub id: String,
    pub lng: f64,
    pub lat: f64,
    pub value: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Highlight {
    pub id: String,
    pub lng: f64,
    pub lat: f64,
}

fn coord(lng: f64, lat: f64) -> Result<(), String> {
    if lng.is_finite()
        && lat.is_finite()
        && (-180.0..=180.0).contains(&lng)
        && (-90.0..=90.0).contains(&lat)
    {
        Ok(())
    } else {
        Err(format!("coordinate out of range: {lng},{lat}"))
    }
}

fn ids<'a>(kind: &str, items: impl Iterator<Item = &'a str>) -> Result<(), String> {
    let mut seen = HashSet::new();
    for (n, id) in items.enumerate() {
        if n >= MAX_ITEMS {
            return Err(format!("{kind}: more than {MAX_ITEMS} items"));
        }
        if id.is_empty() || !seen.insert(id) {
            return Err(format!("{kind}: empty or duplicate id"));
        }
    }
    Ok(())
}

impl MapScene {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(camera) = &self.camera {
            coord(camera.lng, camera.lat)?;
            if let Some(zoom) = camera.zoom
                && !(10.0..=19.0).contains(&zoom)
            {
                return Err("camera zoom must be between 10 and 19".into());
            }
        }
        ids("pins", self.pins.iter().map(|p| p.id.as_str()))?;
        ids("arcs", self.arcs.iter().map(|a| a.id.as_str()))?;
        ids("columns", self.columns.iter().map(|c| c.id.as_str()))?;
        ids("highlights", self.highlights.iter().map(|h| h.id.as_str()))?;
        for pin in &self.pins {
            coord(pin.lng, pin.lat)?;
        }
        for arc in &self.arcs {
            coord(arc.from[0], arc.from[1])?;
            coord(arc.to[0], arc.to[1])?;
        }
        for column in &self.columns {
            coord(column.lng, column.lat)?;
            if !column.value.is_finite() || column.value < 0.0 {
                return Err("column value must be finite and not negative".into());
            }
        }
        for highlight in &self.highlights {
            coord(highlight.lng, highlight.lat)?;
        }
        Ok(())
    }
}
