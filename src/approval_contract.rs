use serde::{Deserialize, Serialize};
use serde_json::Value;
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionEffect {
    Read,
    Write,
    Mixed,
}

impl ExtensionEffect {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Write => "write",
            Self::Mixed => "mixed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        s.parse().ok()
    }

    pub fn is_consequential(&self) -> bool {
        matches!(self, Self::Write | Self::Mixed)
    }
}

impl std::str::FromStr for ExtensionEffect {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "mixed" => Ok(Self::Mixed),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ExtensionCapability {
    pub input_schema: Value,
    #[serde(default)]
    pub supported_regions: Vec<String>,
    pub external_key: String,
    pub display_name: String,
    pub effect: ExtensionEffect,
    #[serde(default)]
    pub consequential: bool,
    #[serde(default)]
    pub data_recipients: Vec<String>,
    #[serde(default)]
    pub access_needs: Vec<String>,
    #[serde(default)]
    pub optional_guarantees: Value,
}

impl ExtensionCapability {
    pub fn optional_guarantees(&self) -> Value {
        self.optional_guarantees.clone()
    }
}
