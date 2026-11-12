//! the transcript. a session is a list of items with stable ids; an item is
//! upserted as it changes (a tool call gains a result). token deltas are not
//! items and are never stored.

use crate::{ItemId, Millis, Seq};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: ItemId,
    /// the tool call this item happened inside (subagent work), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<ItemId>,
    /// seq of the last change to this item.
    pub seq: Seq,
    /// position in the transcript: the seq at which the item first appeared.
    pub order: Seq,
    pub created_ms: Millis,
    #[serde(flatten)]
    pub body: ItemBody,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ItemBody {
    User {
        text: String,
        /// names of the files sent with the message.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<String>,
    },
    Text {
        text: String,
        /// still receiving deltas.
        #[serde(default)]
        streaming: bool,
    },
    Thinking {
        text: String,
        #[serde(default)]
        streaming: bool,
    },
    Tool {
        tool: String,
        input: Value,
        status: ToolStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<String>,
        /// when it actually began running. not when it was asked for: a call
        /// can sit waiting on permission for hours first.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_ms: Option<Millis>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finished_ms: Option<Millis>,
    },
    /// live ui the agent drew (see `crate::surface`). a null `node` is a removed one.
    Surface {
        /// its name. not `id`: an item's body sits in the item's own object, which has one.
        surface: String,
        node: Value,
        #[serde(default = "inline_region")]
        region: String,
    },
    /// a turn ended: how long it ran, what it cost and took, and the files it changed.
    Turn {
        started_ms: Millis,
        finished_ms: Millis,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost_usd: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default)]
        fast: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<TurnUsage>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        files: Vec<TurnFile>,
    },
    /// something the daemon wants in the transcript: interrupted, aborted, error.
    Notice {
        level: NoticeLevel,
        text: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolStatus {
    /// the call is still being written by the model.
    Pending,
    AwaitingPermission,
    Running,
    Ok,
    Failed,
    Denied,
    /// the turn ended (interrupt, crash, restart) before a result arrived.
    Aborted,
    #[serde(other)]
    Unknown,
}

impl ToolStatus {
    pub fn is_open(self) -> bool {
        matches!(self, Self::Pending | Self::AwaitingPermission | Self::Running)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
    #[serde(other)]
    Unknown,
}

fn inline_region() -> String {
    "inline".into()
}

/// what a turn took from the model, in tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnUsage {
    pub input: u64,
    pub output: u64,
    /// read back from the prompt cache.
    pub cache_read: u64,
    /// written to the prompt cache.
    pub cache_write: u64,
}

/// a file a turn changed, and by how many lines.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnFile {
    pub path: String,
    pub added: u32,
    pub removed: u32,
}
