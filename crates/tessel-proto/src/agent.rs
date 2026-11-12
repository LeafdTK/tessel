//! what an agent backend tells the daemon, normalized.
//!
//! a backend (claude today, codex later) turns its own output into these.
//! the daemon never looks at backend-specific json.

use crate::{ItemId, RequestId};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// the agent process is up and told us about itself.
    Init {
        agent_session_id: String,
        model: Option<String>,
        cwd: Option<String>,
        permission_mode: Option<String>,
        agent_version: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mcp_servers: Vec<McpServer>,
    },
    /// the agent's own coarse lifecycle hint. a hint, not the truth: the daemon
    /// derives status from everything it knows.
    Phase { phase: AgentPhase },
    /// a user message the agent accepted (ours, echoed back).
    UserMessage { item_id: ItemId, text: String },
    /// a block began streaming. text arrives as `Delta`s, then a completing
    /// `Text` / `Thinking` / `ToolCall` with the same id.
    BlockStarted {
        item_id: ItemId,
        parent: Option<ItemId>,
        block: BlockKind,
    },
    /// ephemeral. never persisted.
    Delta { item_id: ItemId, text: String },
    Text {
        item_id: ItemId,
        parent: Option<ItemId>,
        text: String,
    },
    Thinking {
        item_id: ItemId,
        parent: Option<ItemId>,
        text: String,
    },
    ToolCall {
        item_id: ItemId,
        parent: Option<ItemId>,
        tool: String,
        input: Value,
    },
    ToolResult {
        item_id: ItemId,
        ok: bool,
        output: String,
    },
    PermissionRequested { request: PermissionRequest },
    /// the agent withdrew a request (interrupt, timeout, answered elsewhere).
    PermissionCancelled { request_id: RequestId },
    /// a long-running unit inside the agent: a subagent or a background command.
    TaskStarted {
        task_id: String,
        tool_item: Option<ItemId>,
        description: String,
        background: bool,
    },
    TaskProgress {
        task_id: String,
        description: String,
    },
    TaskFinished {
        task_id: String,
        status: String,
        summary: Option<String>,
    },
    TurnFinished {
        outcome: TurnOutcome,
        cost_usd: Option<f64>,
        duration_ms: Option<u64>,
        /// the tokens the turn took, as the agent counted them.
        #[serde(default)]
        usage: Option<crate::TurnUsage>,
    },
    RateLimit {
        status: String,
        resets_at: Option<i64>,
        utilization: Option<f64>,
        /// each limit the account has (five hours, a week...), as of now.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        windows: Vec<UsageWindow>,
    },
    /// the tool servers the agent has, and whether each is connected.
    McpServers { servers: Vec<McpServer> },
    /// the agent refused something we asked of it (a model it doesn't have, say).
    RequestFailed { message: String },
    /// the agent is now running as this model.
    ModelChanged { model: String },
    /// the agent's permission mode changed (for example into or out of plan mode).
    ModeChanged { mode: String },
    /// how many tokens the conversation occupies as of the latest reply.
    ContextUsed { tokens: u64 },
    /// how many tokens the current model can hold.
    ContextWindow { tokens: u64 },
    /// the agent process is gone. produced by whoever supervises the process,
    /// never by a parser.
    Exited { code: Option<i32> },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    Running,
    RequiresAction,
    Idle,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlockKind {
    Text,
    Thinking,
    Tool { tool: String },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Error { message: String },
    /// the process died or the daemon restarted with the turn still open.
    Aborted,
    #[serde(other)]
    Unknown,
}

/// the agent is blocked until someone answers this.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub request_id: RequestId,
    /// the tool call this is about, when the agent says.
    pub tool_item: Option<ItemId>,
    pub tool: String,
    pub input: Value,
    pub description: Option<String>,
    /// true when this is the agent asking the user something (a question to
    /// answer) rather than asking leave to do something.
    #[serde(default)]
    pub is_question: bool,
    /// set when the agent has finished planning and wants the plan approved
    /// before it starts work. the plan itself, as markdown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// backend-specific "always allow" style suggestions, passed through.
    #[serde(default)]
    pub suggestions: Vec<Value>,
}

/// one of the account's usage limits, and how much of it is used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// `five_hour`, `seven_day`, `seven_day_opus`...
    pub name: String,
    /// how much is used, 0 to 1.
    pub used: f64,
    /// unix seconds, when it starts over.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<i64>,
    /// `allowed`, `allowed_warning`, `rejected`.
    #[serde(default)]
    pub status: String,
}

/// a tool server the agent is connected to (or failed to connect to).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpServer {
    pub name: String,
    /// `connected`, `pending`, `needs-auth`, `failed`, `disabled`.
    pub status: String,
    /// where it is configured: the user's settings, the project, a plugin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// a model the agent can run as.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// what to ask for: an alias like `sonnet`, or a full id.
    pub value: String,
    /// the full id it stands for, which is how a running session reports its model.
    #[serde(default)]
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// the effort levels it takes (`low` .. `max`); none if effort isn't adjustable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub efforts: Vec<String>,
    /// it can run in fast mode.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
}

/// a slash command the agent knows: its own, a skill's, or the person's.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlashCommand {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// what goes after it, if anything (`<file>`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hint: String,
}

/// what an agent offers: its models, and its slash commands.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Catalog {
    pub models: Vec<ModelInfo>,
    #[serde(default)]
    pub commands: Vec<SlashCommand>,
}

/// every effort level, lowest first.
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// a file sent along with a message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    /// for example `image/png` or `application/pdf`.
    pub media_type: String,
    /// the file's bytes, base64 encoded.
    pub data: String,
}

impl Attachment {
    /// what agents accept as an attachment today. anything else should be
    /// referred to by its path instead.
    pub fn is_supported(&self) -> bool {
        self.media_type.starts_with("image/") || self.media_type == "application/pdf"
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow {
        /// replaces the tool input. also how answers to a question travel.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        updated_input: Option<Value>,
    },
    Deny {
        #[serde(default)]
        message: String,
    },
}
