use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ChartType {
    Line,
    Bar,
    Pie,
    Area,
}

impl ChartType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Line => "line",
            Self::Bar => "bar",
            Self::Pie => "pie",
            Self::Area => "area",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "line" => Some(Self::Line),
            "bar" => Some(Self::Bar),
            "pie" => Some(Self::Pie),
            "area" => Some(Self::Area),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Aggregation {
    Sum,
    Count,
    Avg,
    Min,
    Max,
}

impl Aggregation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sum => "sum",
            Self::Count => "count",
            Self::Avg => "avg",
            Self::Min => "min",
            Self::Max => "max",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "sum" => Some(Self::Sum),
            "count" => Some(Self::Count),
            "avg" => Some(Self::Avg),
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupBy {
    Day,
    Week,
    Month,
    Field(String),
}

impl GroupBy {
    pub fn parse(value: &str) -> Self {
        match value {
            "day" => Self::Day,
            "week" => Self::Week,
            "month" => Self::Month,
            other => Self::Field(other.to_string()),
        }
    }
}

impl Serialize for GroupBy {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            GroupBy::Day => serializer.serialize_str("day"),
            GroupBy::Week => serializer.serialize_str("week"),
            GroupBy::Month => serializer.serialize_str("month"),
            GroupBy::Field(field) => serializer.serialize_str(field),
        }
    }
}

impl<'de> Deserialize<'de> for GroupBy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct GroupByVisitor;

        impl<'de> serde::de::Visitor<'de> for GroupByVisitor {
            type Value = GroupBy;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str(
                    "a time bucket string ('day', 'week', 'month') or a field name or an object",
                )
            }

            fn visit_str<E>(self, value: &str) -> Result<GroupBy, E>
            where
                E: serde::de::Error,
            {
                Ok(GroupBy::parse(value))
            }

            fn visit_map<M>(self, mut access: M) -> Result<GroupBy, M::Error>
            where
                M: serde::de::MapAccess<'de>,
            {
                let mut kind = None;
                let mut field = None;

                while let Some((k, v)) = access.next_entry::<String, String>()? {
                    if k == "type" || k == "kind" {
                        kind = Some(v);
                    } else if k == "field" || k == "name" || k == "value" {
                        field = Some(v);
                    }
                }

                if let Some(f) = field {
                    return Ok(GroupBy::Field(f));
                }

                if let Some(k) = kind {
                    return Ok(GroupBy::parse(&k));
                }

                Ok(GroupBy::Day)
            }
        }

        deserializer.deserialize_any(GroupByVisitor)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, utoipa::ToSchema)]
pub struct QuerySpec {
    pub metric_field: String,
    pub aggregation: Aggregation,
    #[schema(value_type = String)]
    pub group_by: GroupBy,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ChartBoard {
    pub id: Uuid,
    pub user_id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Chart {
    pub id: Uuid,
    pub board_id: Uuid,
    pub title: String,
    pub chart_type: ChartType,
    pub schema_ids: Vec<Uuid>,
    pub query_spec: serde_json::Value,
    pub created_at: DateTime<Utc>,
}
