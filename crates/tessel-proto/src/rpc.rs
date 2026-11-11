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
    /// how hard the agent thinks: one of `EFFORTS`, or none for the model's own default.
    SessionSetEffort {
        cmd_id: Uuid,
        session: SessionId,
        #[serde(default)]
        effort: Option<String>,
    },
    /// fast mode on or off (for models that have it).
    SessionSetFast {
        cmd_id: Uuid,
        session: SessionId,
        fast: bool,
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
        /// the file as one commit changed it, instead of the work so far.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<String>,
        /// against the index instead: `false` what isn't staged (index to
        /// folder), `true` what is (last commit to index).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        index: Option<bool>,
    },
    /// every file in the session's folder that isn't ignored, for finding by name.
    FsIndex {
        session: SessionId,
    },
    /// something to look at for a file that isn't text: an image as it is,
    /// anything else (pdf, video, audio, documents) as a thumbnail the daemon makes.
    FsPreview {
        session: SessionId,
        path: String,
    },
    /// a page of history, newest first.
    GitLog {
        session: SessionId,
        #[serde(default)]
        skip: u32,
        #[serde(default = "default_log_page")]
        limit: u32,
    },
    /// the files one commit changed.
    GitShow {
        session: SessionId,
        sha: String,
    },
    /// what is staged, and what isn't.
    GitWorkTree {
        session: SessionId,
    },
    GitStage {
        session: SessionId,
        paths: Vec<String>,
    },
    GitUnstage {
        session: SessionId,
        paths: Vec<String>,
    },
    /// stage (or unstage) only some lines of a file: lines `start..=end`
    /// of the file as it is now (as staged, for unstaging), 1-based.
    GitStageLines {
        session: SessionId,
        path: String,
        start: u32,
        end: u32,
        #[serde(default)]
        unstage: bool,
        /// the lines are of the old side (removed lines), not the new.
        #[serde(default)]
        old: bool,
    },
    GitCommit {
        session: SessionId,
        message: String,
    },
    /// take back the last thing tessel did to the repository (a stage, an
    /// unstage, a commit), if nothing has moved since.
    GitUndo {
        session: SessionId,
    },
    /// a commit message for what is staged, written by a small fast model.
    GitSuggestMessage {
        session: SessionId,
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
    // ---- windows -----------------------------------------------------------
    //
    // "if you can click it, you can script it": every window registers here and
    // takes commands relayed through the daemon, so a cli, an agent's tools, or
    // a client on another machine drive it through the one door they already use.
    /// this connection is a window, known by `name`.
    UiRegister {
        name: String,
    },
    /// run a window command (`pane.split`, `theme.set`, ...). `window` picks
    /// one by name; none is the most recently registered.
    UiExec {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<String>,
        cmd: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    /// a window's answer to a `UiCall`.
    UiAnswer {
        call: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ok: Option<serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        err: Option<String>,
    },
    /// a window says something happened (`layout_changed`, `pane_focused`, ...).
    UiPublish {
        kind: String,
        #[serde(default)]
        data: serde_json::Value,
    },
    /// hear what windows publish. an empty `kinds` is everything.
    UiSubscribe {
        #[serde(default)]
        kinds: Vec<String>,
    },
    UiWindows,
    // ---- surfaces: ui an agent draws ----------------------------------------
    SurfaceOp {
        session: SessionId,
        op: crate::surface::SurfaceOp,
    },
    /// someone acted on a surface: the agent that drew it hears about it.
    SurfaceEvent {
        session: SessionId,
        surface: String,
        element: String,
        act: String,
        #[serde(default)]
        value: serde_json::Value,
    },
    // ---- terminals -----------------------------------------------------------
    /// start a terminal: a login shell, or `command` run by one.
    TermOpen {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<SessionId>,
        /// where it starts. none: the session's folder, else home.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        command: Vec<String>,
        cols: u16,
        rows: u16,
    },
    TermList,
    /// send me this terminal's bytes: first everything on it (as a binary
    /// frame of the bytes that would draw it), then whatever comes.
    TermAttach {
        term: crate::term::TermId,
    },
    TermDetach {
        term: crate::term::TermId,
    },
    /// type into it. (a window sends keys as binary frames instead.)
    TermInput {
        term: crate::term::TermId,
        text: String,
    },
    TermResize {
        term: crate::term::TermId,
        cols: u16,
        rows: u16,
    },
    /// hang it up. one already exited is forgotten.
    TermClose {
        term: crate::term::TermId,
    },
    /// its text: the screen, or everything history still holds.
    TermCapture {
        term: crate::term::TermId,
        #[serde(default)]
        history: bool,
    },
    /// answer once the screen shows `text`, or (with no text) once it exits.
    TermWait {
        term: crate::term::TermId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        #[serde(default = "default_wait_ms")]
        timeout_ms: u64,
    },
    // ---- the daemon's own settings ------------------------------------------
    Settings,
    /// change some settings: a partial object, merged. answers with all of them.
    SetSettings {
        patch: serde_json::Value,
    },
    Shutdown,
    #[serde(other)]
    Unknown,
}

/// what the daemon is set to. kept with the daemon (not the window) so a
/// remote daemon is set the same way as a local one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonSettings {
    /// agents tessel starts get tessel's tools: arranging the window and
    /// drawing surfaces. takes effect as each agent next starts.
    pub agent_tools: bool,
    /// the models offered first, in order (at most five), each with how it
    /// runs when picked. the rest are a search away.
    pub models: Vec<FavoriteModel>,
}

/// a model in the loadout, and how it runs when picked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FavoriteModel {
    /// what to ask for: an alias like `opus`, or a full id.
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fast: bool,
}

/// the most models in the loadout.
pub const MAX_FAVORITES: usize = 5;

impl Default for DaemonSettings {
    fn default() -> Self {
        let fav = |m: &str, effort: &str| FavoriteModel { model: m.into(), effort: Some(effort.into()), fast: false };
        Self { agent_tools: true, models: vec![fav("opus", "high"), fav("claude-fable-5-2", "high"), fav("sonnet", "high"), fav("haiku", "medium")] }
    }
}

fn default_log_page() -> u32 {
    200
}

fn default_wait_ms() -> u64 {
    30_000
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
    GitLog {
        log: crate::GitLog,
    },
    Paths {
        paths: Vec<String>,
        /// there were more than were sent.
        #[serde(default)]
        more: bool,
    },
    /// an image to show for a file. `data` is base64.
    Preview {
        mime: String,
        data: String,
        /// what it is when the image isn't the file itself ("first page", "a frame").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// the files a commit changed.
    Changed {
        files: Vec<crate::FileChange>,
    },
    WorkTree {
        tree: crate::WorkTree,
    },
    Committed {
        sha: String,
    },
    Suggested {
        message: String,
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
        /// slash commands the agent knows, for a `/` menu.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        commands: Vec<crate::SlashCommand>,
    },
    /// what a window command answered.
    UiResult {
        value: serde_json::Value,
    },
    UiWindows {
        windows: Vec<String>,
    },
    Settings {
        settings: DaemonSettings,
    },
    Term {
        info: crate::term::TermInfo,
    },
    Terms {
        terms: Vec<crate::term::TermInfo>,
    },
    TermText {
        text: String,
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
    // ---- windows ------------------------------------------------------------
    /// to a window: run this command and `UiAnswer` with `call`.
    UiCall {
        call: u64,
        cmd: String,
        #[serde(default)]
        args: serde_json::Value,
    },
    /// to subscribers: something happened in a window.
    Ui {
        window: String,
        kind: String,
        #[serde(default)]
        data: serde_json::Value,
    },
    /// to a window: whether anyone is listening to what it publishes. it
    /// builds no events while nobody is.
    UiWatched {
        watched: bool,
    },
    /// the daemon's settings changed (to everyone connected).
    Settings {
        settings: DaemonSettings,
    },
    /// a terminal opened or changed: title, folder, size, exit (to everyone).
    TermUpdated {
        info: crate::term::TermInfo,
    },
    TermRemoved {
        term: crate::term::TermId,
    },
    /// bytes from a terminal this client is attached to. arrives as a binary
    /// frame, never as json; the client library hands it over in order with
    /// everything else.
    #[serde(skip)]
    TermOutput {
        term: crate::term::TermId,
        bytes: Vec<u8>,
    },
    #[serde(other)]
    Unknown,
}

/// can a client built for `client_major` talk to this daemon?
pub fn compatible(client_major: u16) -> bool {
    client_major == crate::PROTO_MAJOR
}
