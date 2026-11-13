//! what a session is doing, and whether it wants you.
//!
//! two separate things on purpose:
//! - `RunState` is *derived* from facts (pending requests, open turn, open
//!   tools, process alive). it is never "whatever the last event said".
//! - `Attention` is *latched*: it stays until someone marks it seen, and it
//!   lives in the daemon so every client agrees.

use crate::{Millis, PermissionRequest, Seq, SessionId};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RunState {
    /// process is being spawned for a turn.
    Starting,
    Working,
    NeedsPermission,
    /// the agent asked the user a question.
    NeedsInput,
    /// turn finished, process may or may not still be warm.
    Idle,
    /// a turn is open, nothing is in flight, and the agent has been silent too long.
    Stalled,
    /// usage limit hit; `reset_at` is unix seconds when known.
    Limited { reset_at: Option<i64> },
    Errored,
    #[serde(other)]
    Unknown,
}

impl RunState {
    pub fn is_busy(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Working | Self::NeedsPermission | Self::NeedsInput | Self::Stalled
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionReason {
    NeedsPermission,
    NeedsInput,
    Finished,
    Errored,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attention {
    pub reason: AttentionReason,
    /// when it started wanting you. the queue sorts oldest first.
    pub since_ms: Millis,
    /// seq at which it was raised. `MarkSeen` with a seq >= this clears it.
    pub seq: Seq,
}

/// the trace covers this many buckets...
pub const TRACE_BUCKETS: usize = 30;
/// ...each this long: a minute in all.
pub const TRACE_BUCKET_MS: Millis = 2_000;

/// everything the sidebar needs about one session. cheap, sent for all sessions.
///
/// note the shape: independent optional attributes. there is no session kind.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSummary {
    pub id: SessionId,
    /// the "what is this" line.
    pub goal: String,
    /// where the agent runs. opaque to clients: it's a path on the daemon's machine.
    pub cwd: String,
    /// set when the daemon made a worktree for this session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SessionId>,
    pub run_state: RunState,
    /// the live "currently doing" line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<String>,
    /// when the current activity began, for "running tests · 42s".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_since_ms: Option<Millis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    /// is an agent process currently alive for this session.
    #[serde(default)]
    pub warm: bool,
    /// tasks the agent left running in the background.
    #[serde(default)]
    pub background_tasks: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default)]
    pub cost_usd: f64,
    /// the agent's permission mode, when it has told us (`default`, `plan`, `acceptEdits`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// how hard it thinks (one of `EFFORTS`); none: the model's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// fast mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
    /// tokens the conversation occupies, and how many the model can hold.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<u64>,
    /// tool servers the agent has, with their connection status.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<crate::McpServer>,
    /// other folders the agent may read and write besides its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_dirs: Vec<String>,
    /// visual mode: the person watches. the agent's Bash commands show in a
    /// terminal as they run, and it acts in the browser at a pace they can follow.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub visual: bool,
    /// what the agent has running inside it right now: subagents and background commands.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskSummary>,
    /// how much the agent produced in each of the last `TRACE_BUCKETS`
    /// stretches of `TRACE_BUCKET_MS`, oldest first. empty when it has been
    /// silent for the whole window. a flat trace on a "working" session is
    /// the earliest sign that it is stuck.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trace: Vec<u16>,
    /// the oldest plain "may I?" waiting on an answer, which can be answered
    /// without opening the session. questions and plans aren't here: they
    /// need the whole card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asking: Option<crate::RequestId>,
    pub last_seq: Seq,
    pub created_ms: Millis,
    pub updated_ms: Millis,
}

/// a subagent or background command running inside a session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: String,
    pub description: String,
    #[serde(default)]
    pub background: bool,
    /// the tool call in the transcript that started it, to jump to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item: Option<crate::ItemId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeInfo {
    /// the repository the worktree belongs to.
    pub repo: String,
    pub branch: String,
}

/// what a client gets when it opens one session: enough to paint, then live events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionSnapshot {
    pub summary: SessionSummary,
    /// the newest items, oldest first.
    pub items: Vec<crate::Item>,
    /// true when older items exist and can be paged in.
    pub has_more: bool,
    pub pending: Vec<PermissionRequest>,
}
