use serde::{Deserialize, Serialize};

use crate::{Millis, SessionId};

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyInfo {
    pub name: String,
    pub hosts: Vec<String>,
    #[serde(default = "authorization")]
    pub header: String,
    #[serde(default = "bearer")]
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub methods: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paths: Vec<String>,
    #[serde(default)]
    pub stored: bool,
    #[serde(default)]
    pub uses: u32,
}

fn authorization() -> String {
    "authorization".into()
}

fn bearer() -> String {
    "Bearer {key}".into()
}

impl KeyInfo {
    pub fn placeholder(&self) -> String {
        format!("tessel-key-{}", self.name)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeyUse {
    pub at_ms: Millis,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub host: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub path: String,
    pub allowed: bool,
}
