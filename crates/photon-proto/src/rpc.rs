//! requests, responses and events between a client and the daemon.
//!
//! things that must stay true so a remote daemon is not a rewrite:
//! - the client never reads the daemon's filesystem or runs git. paths are
//!   opaque strings that only mean something on the daemon's machine.
//! - every command that changes something carries a client-made `cmd_id`, so
//!   resending after a dropped link is safe.
//! - there are two subscription tiers: a cheap summary of every session, and
//!   the detail of one.

use crate::{
    Item, ItemId, PermissionDecision, PermissionRequest, RequestId, Seq, SessionId, SessionSnapshot,
    SessionSummary,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Envelope {
    Req {
        id: u64,
        req: Request,
    },
    Res {
        id: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<Response>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<RpcError>,
    },
    Ev {
        ev: Event,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case")]
pub enum Request {
    /// must be the first request on a connection.
    Hello {
        proto_major: u16,
        proto_minor: u16,
        build: String,
        client: String,
        /// unused on a local socket. the slot exists so remote doesn't change the handshake.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        auth: Option<String>,
    },
    SessionList,
    SessionCreate {
        cmd_id: Uuid,
        /// a directory on the daemon's machine.
        cwd: String,
        /// when set, the daemon makes a fresh worktree of the repo at `cwd`
        /// and runs the session there instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        worktree: Option<WorktreeSpec>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        goal: Option<String>,
        /// first message. without it the session is created idle.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent: Option<SessionId>,
    },
    SessionSend {
        cmd_id: Uuid,
        session: SessionId,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<crate::Attachment>,
    },
    /// switch the model for the rest of the session (an alias like `sonnet`, or a full id).
    SessionSetModel {
        cmd_id: Uuid,
        session: SessionId,
        model: String,
    },
    /// switch the permission mode: `default`, `plan`, `acceptEdits`, `auto`.
    SessionSetMode {
        cmd_id: Uuid,
        session: SessionId,
        mode: String,
    },
    SessionInterrupt {
        cmd_id: Uuid,
        session: SessionId,
    },
    /// ask the agent for the current state of its tool servers. the answer
    /// arrives as a summary update.
    SessionRefreshMcp {
        session: SessionId,
    },
    /// let the agent work in another folder as well as its own.
    SessionLinkDir {
        cmd_id: Uuid,
        session: SessionId,
        path: String,
    },
    SessionUnlinkDir {
        cmd_id: Uuid,
        session: SessionId,
        path: String,
    },
    /// the models an agent can run as.
    ModelList,
    SessionSetGoal {
        cmd_id: Uuid,
        session: SessionId,
        goal: String,
    },
    SessionDelete {
        cmd_id: Uuid,
        session: SessionId,
        /// also remove the worktree the daemon made, if there is one.
        #[serde(default)]
        remove_worktree: bool,
    },
    PermissionRespond {
        cmd_id: Uuid,
        session: SessionId,
        request_id: RequestId,
        decision: PermissionDecision,
    },
    /// "i have looked at this session up to `seq`". clears attention raised at or before it.
    MarkSeen {
        session: SessionId,
        seq: Seq,
    },
    /// replaces this connection's subscription.
    Subscribe {
        /// receive `SessionUpdated`/`SessionRemoved` for every session.
        #[serde(default)]
        summary: bool,
        /// receive items and deltas for this one session.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<SessionId>,
        /// how many of the newest items to include in the snapshot.
        #[serde(default = "default_tail")]
        tail: u32,
    },
    /// page older items in.
    ItemsBefore {
        session: SessionId,
        before: Seq,
        limit: u32,
    },
    // ---- the session's working directory. paths are relative to it. ----
    /// what has changed since this work started.
    GitStatus {
        session: SessionId,
    },
    /// one changed file, before and after.
    GitDiff {
        session: SessionId,
        path: String,
    },
    /// the entries of a directory (`""` for the top).
    FsList {
        session: SessionId,
        path: String,
    },
    FsRead {
        session: SessionId,
        path: String,
    },
    FsWrite {
        cmd_id: Uuid,
        session: SessionId,
        path: String,
        content: String,
    },
    /// the pull request for this session's branch, and its checks.
    PrStatus {
        session: SessionId,
    },
    Shutdown,
    #[serde(other)]
    Unknown,
}

fn default_tail() -> u32 {
    200
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorktreeSpec {
    /// branch or ref to start from. default: the repo's default remote branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// name for the worktree directory and branch. default: generated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Welcome {
        proto_major: u16,
        proto_minor: u16,
        build: String,
        daemon_pid: u32,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    Sessions {
        sessions: Vec<SessionSummary>,
    },
    Session {
        summary: SessionSummary,
    },
    Subscribed {
        /// present when `summary` was requested.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        sessions: Option<Vec<SessionSummary>>,
        /// present when `detail` was requested.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<SessionSnapshot>,
    },
    Items {
        items: Vec<Item>,
        has_more: bool,
    },
    Git {
        summary: crate::GitSummary,
    },
    Diff {
        diff: crate::FileDiff,
    },
    Dir {
        entries: Vec<crate::DirEntry>,
    },
    File {
        file: crate::FileContent,
    },
    Pr {
        status: crate::PrStatus,
    },
    Models {
        models: Vec<crate::ModelInfo>,
    },
    Ack,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: ErrorCode,
    pub message: String,
}

impl RpcError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// client and daemon speak different protocol majors.
    VersionMismatch,
    /// a request arrived before `Hello`.
    NotReady,
    NotFound,
    Invalid,
    Conflict,
    Internal,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    // summary tier
    SessionUpdated {
        summary: SessionSummary,
    },
    SessionRemoved {
        session: SessionId,
    },
    // detail tier
    ItemUpserted {
        session: SessionId,
        item: Item,
    },
    /// ephemeral streaming text. `offset` is where `text` starts within the
    /// item, in bytes, so a client can tell if it missed some.
    ItemDelta {
        session: SessionId,
        item_id: ItemId,
        offset: u64,
        text: String,
    },
    PendingChanged {
        session: SessionId,
        pending: Vec<PermissionRequest>,
    },
    /// this client fell too far behind and events were dropped. resubscribe.
    Lagged,
    #[serde(other)]
    Unknown,
}

/// can a client built for `client_major` talk to this daemon?
pub fn compatible(client_major: u16) -> bool {
    client_major == crate::PROTO_MAJOR
}
