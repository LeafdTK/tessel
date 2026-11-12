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
    /// follow these sessions in detail (items, deltas, pending prompts), and
    /// no others: replaces what this connection follows. answers with a
    /// snapshot of each one it wasn't following already. a window showing
    /// several chats at once follows them all.
    Follow {
        sessions: Vec<SessionId>,
        /// how many of the newest items each new snapshot carries.
        #[serde(default = "default_tail")]
        tail: u32,
    },
    /// what a session's repository says its workspaces need: setup, run
    /// buttons, files to copy. read from `.tessel/settings.toml` (shared,
    /// committed) and `.tessel/settings.local.toml` (this machine's).
    Scripts {
        session: SessionId,
    },
    /// a step towards the main branch for a session's work: push, pull
    /// request, merge, update, or straight into main.
    Ship {
        session: SessionId,
        step: crate::workspace::ShipStep,
    },
    /// put a session's files back as they were before one of its turns (a
    /// `Turn` item); or, with no turn, take back the last time that was done.
    Rewind {
        session: SessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<crate::ItemId>,
    },
    /// do something to a session's page (see `BrowserAct`). `agent`: the
    /// agent did it, so watchers are shown.
    Browser {
        session: SessionId,
        act: crate::BrowserAct,
        #[serde(default)]
        agent: bool,
    },
    /// restart the daemon without stopping what it runs: terminals and idle
    /// agents carry on under a new daemon (`bin`, else the same binary, as it
    /// is on disk now). agents mid-turn are waited for, up to `wait_secs`
    /// (ten minutes when not given); `now` cuts them short instead, as a
    /// crash would. answered once the new daemon has everything.
    DaemonRestart {
        #[serde(default)]
        now: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        wait_secs: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bin: Option<String>,
    },
    /// this window shows pages itself, natively: what agents do in a
    /// session's browser is relayed to it (`Event::BrowserCall`) while it's
    /// open, and the daemon's own engine is only for when no window is.
    BrowserHost {
        on: bool,
    },
    /// which browser engine the daemon would drive, if any.
    BrowserEngine,
    /// the account's usage limits, as the agent last said.
    Usage,
    /// download one (Chrome for Testing) into tessel's own folder.
    BrowserInstall,
    /// write a repository's scripts. `local`: in the main checkout's
    /// `.tessel/settings.local.toml`, for this machine only; otherwise in the
    /// workspace's `.tessel/settings.toml`, to be committed and shared.
    ScriptsSave {
        session: SessionId,
        scripts: WorkspaceScripts,
        local: bool,
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
    /// a new session that carries on from another's conversation: in the same
    /// workspace (a new tab), or in a new worktree branched from where it is.
    SessionFork {
        cmd_id: Uuid,
        session: SessionId,
        /// a new workspace (worktree) rather than a tab in the same one.
        #[serde(default)]
        workspace: bool,
    },
    /// what a folder is, before there's a session in it: whether it's in a git
    /// repository (so a session there can have a worktree of its own).
    FolderInfo {
        path: String,
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

/// a button that runs something in a workspace: a dev server, the tests.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptAction {
    /// its name in the file (`dev`), which is also what the button says.
    pub id: String,
    pub command: String,
    /// a lucide icon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    /// the one the run button starts.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub default: bool,
}

/// what a repository says each of its workspaces needs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceScripts {
    /// run once, when a workspace is made (installing dependencies, say).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// run before a workspace is removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<ScriptAction>,
    /// gitignored files a new workspace gets a copy of (`.env*`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copy: Vec<String>,
    /// several workspaces may run the same script at once (each on its own
    /// ports). false: starting one stops it in the repository's other
    /// workspaces, for a script that needs a fixed port or a shared database.
    #[serde(default = "yes")]
    pub concurrent: bool,
    /// the files this was read from, most important last. empty: nothing set up yet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from: Vec<String>,
}

fn yes() -> bool {
    true
}

impl Default for WorkspaceScripts {
    fn default() -> Self {
        Self { setup: None, archive: None, actions: Vec::new(), copy: Vec::new(), concurrent: true, from: Vec::new() }
    }
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
    Scripts {
        scripts: WorkspaceScripts,
        /// the first of the ten ports the workspace has (`$TESSEL_PORT`).
        port: u16,
    },
    /// what a ship step did, in words, and a link if it made one (a pull request).
    Shipped {
        said: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Browser {
        state: crate::BrowserState,
        /// what reading the page found.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// a screenshot, PNG, base64.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        image: Option<String>,
    },
    BrowserEngine {
        engine: crate::BrowserEngine,
    },
    /// the daemon handed over to a new one: its pid, and what carried on.
    Restarted {
        pid: u32,
        terminals: u32,
        agents: u32,
        /// agents that were mid-turn and were cut short.
        cut_short: u32,
    },
    Usage {
        windows: Vec<crate::UsageWindow>,
    },
    Followed {
        /// the sessions newly followed, as they are now.
        snapshots: Vec<SessionSnapshot>,
        /// asked for, but there's no such session (deleted, say).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        missing: Vec<SessionId>,
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
    FolderInfo {
        /// the repository's top folder, when the folder is inside one.
        git_root: Option<String>,
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
    /// to a window that hosts pages: do this to a session's page and
    /// `UiAnswer` with `call`, the answer being a `Response::Browser`'s fields.
    /// `first`: where a new page starts.
    BrowserCall {
        call: u64,
        session: SessionId,
        act: crate::BrowserAct,
        #[serde(default)]
        agent: bool,
        #[serde(default)]
        first: String,
    },
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
    /// a frame of a session's page, to those watching it. JPEG, base64.
    BrowserFrame {
        session: SessionId,
        jpeg: String,
        width: u32,
        height: u32,
    },
    /// where a session's page is now (to everyone).
    BrowserState {
        session: SessionId,
        state: crate::BrowserState,
    },
    /// the person pointed at an element (to everyone).
    BrowserPicked {
        session: SessionId,
        picked: crate::Picked,
    },
    /// the agent did something to the page: what, and where (to everyone).
    BrowserActivity {
        session: SessionId,
        what: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
    },
    /// the account's usage changed (to everyone).
    Usage {
        windows: Vec<crate::UsageWindow>,
    },
    /// the engine changed: found, installing, installed (to everyone).
    BrowserEngine {
        engine: crate::BrowserEngine,
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
