use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for SessionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::str::FromStr for SessionId {
    type Err = uuid::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(s)?))
    }
}

/// stable id of one transcript item. for tool calls this is the agent's own
/// tool-use id, so a result can find its call.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ItemId(pub String);

impl ItemId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ItemId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl From<String> for ItemId {
    fn from(s: String) -> Self {
        Self(s)
    }
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// id of a pending permission/question request, as issued by the agent.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RequestId(pub String);

impl From<&str> for RequestId {
    fn from(s: &str) -> Self {
        Self(s.to_owned())
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// per-session monotonic counter. every persisted change to a session bumps it.
pub type Seq = u64;
