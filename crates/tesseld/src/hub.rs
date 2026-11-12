//! the one task that owns every session.
//!
//! everything that can change a session arrives here as a message: client
//! requests, agent output, timer ticks. one task, one queue, no locks, so
//! there is exactly one order in which things happened.

use crate::paths::Paths;
use crate::server::Out;
use tessel_proto::term::TermId;
use crate::reducer::{Effects, Input, Meta, SessionState};
use crate::store::Store;
use crate::workspace;
use crate::worktree;
use tessel_agent::{Agent, AgentOutput, Backend, SpawnOpts};
use tessel_proto::{Attachment, 
    AgentEvent, Envelope, ErrorCode, Event, ItemId, Millis, PROTO_MAJOR, PROTO_MINOR, Request, Response, RpcError,
    SessionId, SessionSummary, WorktreeInfo, WorktreeSpec, compatible,
};
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

pub type ClientId = u64;

/// how many envelopes may queue for one client before we give up on it.
pub const CLIENT_QUEUE: usize = 8192;

/// remembered command ids, so a resend after a dropped link is a no-op.
const REMEMBERED_COMMANDS: usize = 4096;

#[derive(Clone)]
pub struct Config {
    pub paths: Paths,
    pub build: String,
    /// stop an agent process that has had nothing to do for this long. the
    /// session stays; its next message starts a new process that resumes it.
    pub idle_reap: Duration,
    /// how long an interrupt gets before we stop the process outright.
    pub interrupt_grace: Duration,
    pub tick: Duration,
    /// `tesselctl`, to give every agent tessel's tools. none: agents get none.
    pub tools_bin: Option<std::path::PathBuf>,
}

impl Config {
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            build: env!("CARGO_PKG_VERSION").to_owned(),
            idle_reap: Duration::from_secs(15 * 60),
            interrupt_grace: Duration::from_secs(8),
            tick: Duration::from_millis(500),
            tools_bin: None,
        }
    }
}

pub enum HubMsg {
    Connected { client: ClientId, tx: mpsc::Sender<Out> },
    Request { client: ClientId, id: u64, req: Request },
    Disconnected { client: ClientId },
    Agent { session: SessionId, generation: u64, output: AgentOutput },
    /// the answer to a request that needed work off the main loop (git, files, the network).
    Reply { client: ClientId, id: u64, result: Result<Response, RpcError> },
    /// a worktree finished being made (or failed) for a pending create.
    WorktreeReady { client: ClientId, id: u64, create: Box<Create>, result: Result<worktree::Created, RpcError> },
    /// keys for a terminal: a binary frame from any client.
    TermInput { payload: Vec<u8> },
    /// from a terminal's thread.
    TermOutput { term: TermId, bytes: Vec<u8> },
    TermChanged { term: TermId, title: String, cwd: Option<String> },
    TermExited { term: TermId, code: i32 },
    /// everything on a terminal, for a client that asked to attach.
    TermDump { term: TermId, client: ClientId, id: u64, bytes: Vec<u8> },
    Stop,
}

pub struct Create {
    cmd_id: Uuid,
    cwd: String,
    goal: Option<String>,
    prompt: Option<String>,
    model: Option<String>,
    parent: Option<SessionId>,
    fork: Option<Fork>,
}

/// what a forked session carries on from.
pub struct Fork {
    /// the parent's conversation, as the agent knows it.
    agent_id: String,
    from_cwd: String,
    items: Vec<tessel_proto::Item>,
    effort: Option<String>,
    fast: bool,
    permission_mode: Option<String>,
    /// the parent's repository, so a new worktree's project is the same one.
    repo: Option<String>,
}

struct Client {
    tx: mpsc::Sender<Out>,
    ready: bool,
    summary: bool,
    /// the sessions it follows in detail.
    detail: Vec<SessionId>,
    /// we dropped events for this client and still owe it a `Lagged`.
    lagged: bool,
    dead: bool,
    /// set when this connection is a window: its name, and when it registered.
    window: Option<(String, u64)>,
    /// set when it listens to what windows publish: the kinds it wants (empty: all).
    ui_kinds: Option<Vec<String>>,
}

/// a window command on its way to a window and back.
struct UiCall {
    client: ClientId,
    id: u64,
    window: ClientId,
    deadline: Millis,
}

/// how long a window has to answer a command before the caller is told it didn't.
const UI_CALL_TIMEOUT_MS: Millis = 10_000;

struct Live {
    state: SessionState,
    agent: Option<Box<dyn Agent>>,
    /// bumps on every spawn, so output from an old process is ignored.
    generation: u64,
    last_summary: Option<SessionSummary>,
    interrupt_deadline: Option<Millis>,
    /// something only a fresh start picks up has changed (a linked folder).
    /// the agent is stopped as soon as it is idle and comes back on demand.
    restart_when_idle: bool,
    /// a fork's parent conversation, until the agent has started one of its own.
    fork_from: Option<String>,
}

pub struct Hub {
    cfg: Config,
    store: Store,
    backend: Arc<dyn Backend>,
    tx: mpsc::UnboundedSender<HubMsg>,
    sessions: HashMap<SessionId, Live>,
    clients: HashMap<ClientId, Client>,
    done: HashMap<Uuid, Response>,
    done_order: VecDeque<Uuid>,
    ui_calls: HashMap<u64, UiCall>,
    next_ui: u64,
    settings: tessel_proto::DaemonSettings,
    terms: std::collections::BTreeMap<TermId, crate::term::Handle>,
    next_term: TermId,
    /// what tessel did to each session's repository, newest last: what undo takes back.
    git_undo: GitUndoStacks,
}

type GitUndoStacks = Arc<std::sync::Mutex<HashMap<SessionId, Vec<GitUndo>>>>;

/// something tessel did to a repository, and how to take it back.
enum GitUndo {
    /// index entries as they were before, and as the change left them.
    Index { what: String, before: Vec<(String, Option<(String, String)>)>, after: Vec<(String, Option<(String, String)>)> },
    Commit { what: String, sha: String },
}

impl GitUndo {
    fn what(&self) -> &str {
        match self {
            GitUndo::Index { what, .. } | GitUndo::Commit { what, .. } => what,
        }
    }
}

/// the work tree, with what undo would take back.
async fn tree_with_undo(dir: &Path, undo: &GitUndoStacks, session: SessionId) -> tessel_proto::WorkTree {
    let mut tree = workspace::work_tree(dir).await;
    tree.undo = undo.lock().unwrap().get(&session).and_then(|s| s.last()).map(|u| u.what().to_owned());
    tree
}

fn short_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn names(paths: &[String]) -> String {
    match paths {
        [one] => short_name(one).to_owned(),
        many => format!("{} files", many.len()),
    }
}

pub fn now_ms() -> Millis {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as Millis).unwrap_or(0)
}

impl Hub {
    /// load everything from the store. sessions that were mid-turn when the
    /// daemon last stopped come back closed and flagged.
    pub async fn load(
        cfg: Config,
        store: Store,
        backend: Arc<dyn Backend>,
        tx: mpsc::UnboundedSender<HubMsg>,
    ) -> anyhow::Result<Self> {
        let mut hub = Self {
            cfg,
            store,
            backend,
            tx,
            sessions: HashMap::new(),
            clients: HashMap::new(),
            done: HashMap::new(),
            done_order: VecDeque::new(),
            ui_calls: HashMap::new(),
            next_ui: 1,
            settings: tessel_proto::DaemonSettings::default(),
            terms: Default::default(),
            next_term: 1,
            git_undo: Default::default(),
        };
        hub.settings = load_settings(&hub.cfg.paths.settings());
        let now = now_ms();
        for p in hub.store.load_sessions().await? {
            let id = p.meta.id;
            let items = hub.store.load_items(id).await?;
            // an agent left running by a daemon that was killed is stopped
            // before anything else can talk to its session.
            let dir = hub.cfg.paths.session_dir(&id);
            let sid = id.to_string();
            let markers = [sid.as_str(), p.agent_session_id.as_deref().unwrap_or("")];
            if let Some(pid) = crate::orphans::reap(&dir, &markers) {
                eprintln!("tesseld: stopped agent {pid} left over from a previous daemon (session {id})");
            }
            let mut state = SessionState::restore(p, items);
            let fx = state.apply(Input::Recovered, now);
            if !fx.upserts.is_empty() || fx.pending_changed || fx.summary_changed {
                hub.store.save(state.persisted(), fx.upserts);
            }
            hub.sessions.insert(id, Live { state, agent: None, generation: 0, last_summary: None, interrupt_deadline: None, restart_when_idle: false, fork_from: None });
        }
        Ok(hub)
    }

    pub async fn run(mut self, mut rx: mpsc::UnboundedReceiver<HubMsg>) {
        let mut tick = tokio::time::interval(self.cfg.tick);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                msg = rx.recv() => match msg {
                    None | Some(HubMsg::Stop) => break,
                    Some(msg) => {
                        if self.handle(msg) {
                            break;
                        }
                    }
                },
                _ = tick.tick() => self.tick(),
            }
        }
        self.shutdown().await;
    }

    async fn shutdown(&mut self) {
        for live in self.sessions.values_mut() {
            if let Some(agent) = live.agent.take() {
                agent.kill();
            }
        }
        self.store.flush().await;
    }

    /// returns true when the daemon should stop.
    fn handle(&mut self, msg: HubMsg) -> bool {
        match msg {
            HubMsg::Connected { client, tx } => {
                self.clients.insert(client, Client { tx, ready: false, summary: false, detail: Vec::new(), lagged: false, dead: false, window: None, ui_kinds: None });
            }
            HubMsg::Disconnected { client } => {
                let gone = self.clients.remove(&client);
                for t in self.terms.values_mut() {
                    t.attached.remove(&client);
                }
                // a window that goes away answers nothing more: its callers hear so now.
                if gone.as_ref().is_some_and(|c| c.window.is_some()) {
                    let stranded: Vec<u64> = self.ui_calls.iter().filter(|(_, c)| c.window == client).map(|(k, _)| *k).collect();
                    for call in stranded {
                        if let Some(c) = self.ui_calls.remove(&call) {
                            self.respond(c.client, c.id, Err(RpcError::new(ErrorCode::NotFound, "the window closed before it answered")));
                        }
                    }
                }
                if gone.is_some_and(|c| c.ui_kinds.is_some()) {
                    self.tell_windows_if_watched();
                }
            }
            HubMsg::Request { client, id, req } => {
                let stop = matches!(req, Request::Shutdown);
                if let Some(result) = self.request(client, id, req) {
                    self.respond(client, id, result);
                }
                return stop && self.clients.get(&client).is_some_and(|c| c.ready);
            }
            HubMsg::Agent { session, generation, output } => self.agent_output(session, generation, output),
            HubMsg::Reply { client, id, result } => self.respond(client, id, result),
            HubMsg::WorktreeReady { client, id, create, result } => {
                let result = result.and_then(|made| {
                    // a fork's worktree is of the parent's repository, whichever folder it was made from.
                    let repo = create.fork.as_ref().and_then(|f| f.repo.clone()).unwrap_or(made.repo);
                    let wt = WorktreeInfo { repo, branch: made.branch };
                    self.create_session(*create, made.path, Some(wt))
                });
                self.respond(client, id, result);
            }
            HubMsg::TermInput { payload } => {
                if let Some((term, bytes)) = tessel_proto::term::decode(&payload)
                    && let Some(t) = self.terms.get(&term)
                {
                    t.send(crate::term::Cmd::Input(bytes.to_vec()));
                }
            }
            HubMsg::TermOutput { term, bytes } => {
                let to: Vec<ClientId> = self.terms.get(&term).map(|t| t.attached.iter().copied().collect()).unwrap_or_default();
                for c in to {
                    self.send(c, Out::Term(term, bytes.clone()));
                }
            }
            HubMsg::TermChanged { term, title, cwd } => {
                if let Some(t) = self.terms.get_mut(&term) {
                    t.info.title = title;
                    if let Some(cwd) = cwd {
                        t.info.cwd = cwd;
                    }
                    let info = t.info.clone();
                    self.tell_everyone(Event::TermUpdated { info });
                }
            }
            HubMsg::TermExited { term, code } => {
                if let Some(t) = self.terms.get_mut(&term) {
                    t.info.exited = Some(code);
                    if t.closing {
                        self.forget_term(term);
                    } else {
                        let info = t.info.clone();
                        self.tell_everyone(Event::TermUpdated { info });
                    }
                }
            }
            HubMsg::TermDump { term, client, id, bytes } => match self.terms.get_mut(&term) {
                Some(t) if self.clients.contains_key(&client) => {
                    t.attached.insert(client);
                    let info = t.info.clone();
                    self.respond(client, id, Ok(Response::Term { info }));
                    self.send(client, Out::Term(term, bytes));
                }
                Some(_) => {}
                None => self.respond(client, id, Err(no_term(term))),
            },
            HubMsg::Stop => {
                for t in self.terms.values() {
                    t.send(crate::term::Cmd::Quit);
                }
                return true;
            }
        }
        self.clients.retain(|_, c| !c.dead);
        false
    }

    // ---- requests ------------------------------------------------------

    /// `None` means the answer will come later (a worktree is being made).
    fn request(&mut self, client: ClientId, id: u64, req: Request) -> Option<Result<Response, RpcError>> {
        let ready = self.clients.get(&client).is_some_and(|c| c.ready);
        if !ready && !matches!(req, Request::Hello { .. }) {
            return Some(Err(RpcError::new(ErrorCode::NotReady, "say hello first")));
        }
        Some(match req {
            Request::Hello { proto_major, proto_minor, .. } => {
                if !compatible(proto_major) {
                    Err(RpcError::new(
                        ErrorCode::VersionMismatch,
                        format!(
                            "this daemon speaks protocol {PROTO_MAJOR}.{PROTO_MINOR}; the client speaks {proto_major}.{proto_minor}. \
                             restart the daemon (or update the client) so they match."
                        ),
                    ))
                } else {
                    if let Some(c) = self.clients.get_mut(&client) {
                        c.ready = true;
                    }
                    Ok(Response::Welcome {
                        proto_major: PROTO_MAJOR,
                        proto_minor: PROTO_MINOR,
                        build: self.cfg.build.clone(),
                        daemon_pid: std::process::id(),
                        capabilities: vec!["worktrees".into(), "replay-user".into()],
                    })
                }
            }
            Request::SessionList => Ok(Response::Sessions { sessions: self.summaries() }),
            Request::SessionCreate { cmd_id, cwd, worktree, goal, prompt, model, parent } => {
                if let Some(prev) = self.done.get(&cmd_id) {
                    return Some(Ok(prev.clone()));
                }
                if !Path::new(&cwd).is_dir() {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, format!("{cwd} is not a directory"))));
                }
                if let Some(p) = parent
                    && !self.sessions.contains_key(&p)
                {
                    return Some(Err(RpcError::new(ErrorCode::NotFound, format!("parent session {p} does not exist"))));
                }
                let create = Create { cmd_id, cwd: cwd.clone(), goal, prompt, model, parent, fork: None };
                match worktree {
                    None => self.create_session(create, cwd, None),
                    Some(spec) => {
                        self.make_worktree(client, id, create, spec);
                        return None;
                    }
                }
            }
            Request::SessionSend { cmd_id, session, text, attachments } => {
                if self.done.contains_key(&cmd_id) {
                    return Some(Ok(Response::Ack));
                }
                let r = self.send_user(session, cmd_id, text, attachments);
                if r.is_ok() {
                    self.remember(cmd_id, Response::Ack);
                }
                r.map(|_| Response::Ack)
            }
            // with an agent running, ask it and let its answer change what we
            // show: it knows the model's real name and may refuse. without
            // one, remember the choice for when the next one starts.
            Request::SessionSetModel { session, model, .. } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(_) if model.trim().is_empty() => Err(RpcError::new(ErrorCode::Invalid, "no model given")),
                Some(live) => match &live.agent {
                    Some(agent) => {
                        agent.set_model(&model);
                        Ok(Response::Ack)
                    }
                    None => self.apply(session, Input::SetModel { model }).map(|_| Response::Ack),
                },
            },
            Request::SessionSetEffort { session, effort, .. } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(_) if effort.as_deref().is_some_and(|e| !tessel_proto::EFFORTS.contains(&e)) => {
                    Err(RpcError::new(ErrorCode::Invalid, format!("effort is one of {}", tessel_proto::EFFORTS.join(", "))))
                }
                Some(live) => {
                    // a running agent changes now; either way it's kept for the next start.
                    if let Some(agent) = &live.agent {
                        agent.apply_settings(&serde_json::json!({"effortLevel": effort}));
                    }
                    self.apply(session, Input::SetEffort { effort }).map(|_| Response::Ack)
                }
            },
            Request::SessionSetFast { session, fast, .. } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(live) => {
                    if let Some(agent) = &live.agent {
                        agent.apply_settings(&serde_json::json!({"fastMode": fast}));
                    }
                    self.apply(session, Input::SetFast { fast }).map(|_| Response::Ack)
                }
            },
            Request::SessionSetMode { session, mode, .. } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(_) if mode.trim().is_empty() => Err(RpcError::new(ErrorCode::Invalid, "no mode given")),
                Some(live) => match &live.agent {
                    Some(agent) => {
                        agent.set_mode(&mode);
                        Ok(Response::Ack)
                    }
                    None => self.apply(session, Input::SetMode { mode }).map(|_| Response::Ack),
                },
            },
            Request::SessionInterrupt { session, .. } => match self.sessions.get_mut(&session) {
                None => Err(not_found(session)),
                Some(live) => {
                    if let Some(agent) = &live.agent
                        && live.state.turn_open()
                    {
                        agent.interrupt();
                        live.interrupt_deadline = Some(now_ms() + self.cfg.interrupt_grace.as_millis() as Millis);
                    }
                    Ok(Response::Ack)
                }
            },
            Request::SessionRefreshMcp { session } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(live) => {
                    // with nothing running there is nobody to ask; what we last heard stands.
                    if let Some(agent) = &live.agent {
                        agent.mcp_status();
                    }
                    Ok(Response::Ack)
                }
            },
            Request::SessionLinkDir { session, path, .. } => {
                let path = path.trim().to_owned();
                if !self.sessions.contains_key(&session) {
                    Err(not_found(session))
                } else if !Path::new(&path).is_absolute() || !Path::new(&path).is_dir() {
                    Err(RpcError::new(ErrorCode::Invalid, format!("{path} is not a folder that exists (give its full path)")))
                } else {
                    self.relink(session, Input::LinkDir { path })
                }
            }
            Request::SessionUnlinkDir { session, path, .. } => self.relink(session, Input::UnlinkDir { path }),
            Request::ModelList => {
                let (models, tx) = (self.backend.models(), self.tx.clone());
                tokio::spawn(async move {
                    let result = models.await.map(|c| Response::Models { models: c.models, commands: c.commands }).map_err(|e| RpcError::new(ErrorCode::Internal, format!("{e:#}")));
                    let _ = tx.send(HubMsg::Reply { client, id, result });
                });
                return None;
            }
            Request::SessionSetGoal { session, goal, .. } => {
                self.apply(session, Input::SetGoal { goal }).map(|_| Response::Ack)
            }
            Request::SessionDelete { session, remove_worktree, .. } => match self.sessions.remove(&session) {
                // deleting something already gone is fine: that's what a resend looks like.
                None => Ok(Response::Ack),
                Some(live) => {
                    if let Some(agent) = &live.agent {
                        agent.kill();
                    }
                    self.store.delete(session);
                    let dir = self.cfg.paths.session_dir(&session);
                    let wt = live.state.meta.worktree.clone().filter(|_| remove_worktree);
                    let path = live.state.meta.cwd.clone();
                    tokio::spawn(async move {
                        let _ = tokio::fs::remove_dir_all(dir).await;
                        if let Some(wt) = wt
                            && let Err(e) = worktree::remove(&wt.repo, &path).await
                        {
                            eprintln!("tesseld: removing worktree {path}: {e}");
                        }
                    });
                    self.broadcast_summary_event(Event::SessionRemoved { session }, Some(session));
                    for c in self.clients.values_mut() {
                        c.detail.retain(|s| *s != session);
                    }
                    Ok(Response::Ack)
                }
            },
            Request::PermissionRespond { session, request_id, decision, .. } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(live) => match (live.state.pending_request(&request_id).cloned(), &live.agent) {
                    // already answered (by another client, or by a resend).
                    (None, _) => Ok(Response::Ack),
                    (Some(_), None) => Err(RpcError::new(ErrorCode::Conflict, "the agent is no longer running")),
                    (Some(req), Some(agent)) => {
                        agent.answer(&req, &decision);
                        self.apply(session, Input::Answered { request_id, decision }).map(|_| Response::Ack)
                    }
                },
            },
            Request::MarkSeen { session, seq } => self.apply(session, Input::MarkSeen { seq }).map(|_| Response::Ack),
            Request::Subscribe { summary, detail, tail } => {
                let now = now_ms();
                let snapshot = match detail {
                    None => None,
                    Some(s) => match self.sessions.get(&s) {
                        None => return Some(Err(not_found(s))),
                        Some(live) => Some(live.state.snapshot(tail as usize, now)),
                    },
                };
                if let Some(c) = self.clients.get_mut(&client) {
                    c.summary = summary;
                    c.detail = detail.into_iter().collect();
                    c.lagged = false;
                }
                Ok(Response::Subscribed { sessions: summary.then(|| self.summaries()), detail: snapshot })
            }
            Request::Follow { sessions, tail } => {
                let now = now_ms();
                let before = self.clients.get(&client).map(|c| c.detail.clone()).unwrap_or_default();
                let (mut follow, mut snapshots, mut missing) = (Vec::new(), Vec::new(), Vec::new());
                for s in sessions {
                    if follow.contains(&s) {
                        continue;
                    }
                    match self.sessions.get(&s) {
                        None => missing.push(s),
                        Some(live) => {
                            if !before.contains(&s) {
                                snapshots.push(live.state.snapshot(tail as usize, now));
                            }
                            follow.push(s);
                        }
                    }
                }
                if let Some(c) = self.clients.get_mut(&client) {
                    c.detail = follow;
                }
                Ok(Response::Followed { snapshots, missing })
            }
            Request::ItemsBefore { session, before, limit } => match self.sessions.get(&session) {
                None => Err(not_found(session)),
                Some(live) => {
                    let (items, has_more) = live.state.items_before(before, limit.min(2000) as usize);
                    Ok(Response::Items { items, has_more })
                }
            },
            // everything about the working directory runs off the main loop:
            // git and the disk can be slow, and one slow repository must not
            // stall every other session.
            Request::GitStatus { session } => {
                return self.in_workdir(client, id, session, |dir| async move { Ok(Response::Git { summary: workspace::git_status(&dir).await }) });
            }
            Request::GitDiff { session, path, at, index } => {
                return self.in_workdir(client, id, session, move |dir| async move {
                    let diff = match index {
                        Some(staged) => workspace::git_diff_index(&dir, &path, staged).await,
                        None => workspace::git_diff(&dir, &path, at.as_deref()).await,
                    };
                    diff.map(|diff| Response::Diff { diff })
                });
            }
            Request::SessionFork { cmd_id, session, workspace } => {
                if let Some(prev) = self.done.get(&cmd_id) {
                    return Some(Ok(prev.clone()));
                }
                let Some(parent) = self.sessions.get(&session) else { return Some(Err(not_found(session))) };
                let st = &parent.state;
                let Some(agent_id) = st.agent_session_id.clone() else {
                    return Some(Err(RpcError::new(ErrorCode::Conflict, "there's no conversation to fork yet: send it a message first")));
                };
                let fork = Fork {
                    agent_id,
                    from_cwd: st.meta.cwd.clone(),
                    items: st.items().to_vec(),
                    effort: st.effort.clone(),
                    fast: st.fast,
                    permission_mode: st.permission_mode.clone(),
                    repo: st.meta.worktree.as_ref().map(|w| w.repo.clone()),
                };
                let create = Create { cmd_id, cwd: st.meta.cwd.clone(), goal: Some(format!("{} (fork)", st.meta.goal)), prompt: None, model: st.model.clone(), parent: None, fork: Some(fork) };
                if workspace {
                    // a worktree branched from where this one is now: its commit, not the repo's default.
                    let spec = WorktreeSpec { base: Some("HEAD".into()), name: None };
                    self.make_worktree(client, id, create, spec);
                    return None;
                }
                let cwd = create.cwd.clone();
                let wt = self.sessions[&session].state.meta.worktree.clone();
                self.create_session(create, cwd, wt)
            }
            Request::FolderInfo { path } => {
                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let git_root = worktree::repo_root(&path).await;
                    let _ = tx.send(HubMsg::Reply { client, id, result: Ok(Response::FolderInfo { git_root }) });
                });
                return None;
            }
            Request::FsIndex { session } => {
                return self.in_workdir(client, id, session, |dir| async move { workspace::index(&dir).await.map(|(paths, more)| Response::Paths { paths, more }) });
            }
            Request::FsPreview { session, path } => {
                return self.in_workdir(client, id, session, |dir| async move {
                    use base64::Engine as _;
                    let (mime, bytes, note) = workspace::preview(&dir, &path).await?;
                    Ok(Response::Preview { mime, data: base64::engine::general_purpose::STANDARD.encode(bytes), note })
                });
            }
            Request::GitLog { session, skip, limit } => {
                return self.in_workdir(client, id, session, move |dir| async move { workspace::git_log(&dir, skip, limit).await.map(|log| Response::GitLog { log }) });
            }
            Request::GitShow { session, sha } => {
                return self.in_workdir(client, id, session, |dir| async move { workspace::git_show(&dir, &sha).await.map(|files| Response::Changed { files }) });
            }
            Request::GitWorkTree { session } => {
                let undo = self.git_undo.clone();
                return self.in_workdir(client, id, session, move |dir| async move { Ok(Response::WorkTree { tree: tree_with_undo(&dir, &undo, session).await }) });
            }
            req @ (Request::GitStage { .. } | Request::GitUnstage { .. } | Request::GitStageLines { .. }) => {
                let undo = self.git_undo.clone();
                let (session, paths, what) = match &req {
                    Request::GitStage { session, paths } => (*session, paths.clone(), format!("Stage {}", names(paths))),
                    Request::GitUnstage { session, paths } => (*session, paths.clone(), format!("Unstage {}", names(paths))),
                    Request::GitStageLines { session, path, start, end, unstage, .. } => {
                        (*session, vec![path.clone()], format!("{} lines {start}–{end} of {}", if *unstage { "Unstage" } else { "Stage" }, short_name(path)))
                    }
                    _ => unreachable!(),
                };
                return self.in_workdir(client, id, session, move |dir| async move {
                    let before = workspace::index_entries(&dir, &paths).await;
                    match req {
                        Request::GitStage { paths, .. } => workspace::stage(&dir, &paths).await?,
                        Request::GitUnstage { paths, .. } => workspace::unstage(&dir, &paths).await?,
                        Request::GitStageLines { path, start, end, unstage, old, .. } => workspace::stage_lines(&dir, &path, start, end, unstage, old).await?,
                        _ => unreachable!(),
                    }
                    let after = workspace::index_entries(&dir, &paths).await;
                    if before != after {
                        undo.lock().unwrap().entry(session).or_default().push(GitUndo::Index { what, before, after });
                    }
                    Ok(Response::WorkTree { tree: tree_with_undo(&dir, &undo, session).await })
                });
            }
            Request::GitCommit { session, message } => {
                let undo = self.git_undo.clone();
                return self.in_workdir(client, id, session, move |dir| async move {
                    let sha = workspace::commit(&dir, &message).await?;
                    let subject = message.lines().next().unwrap_or("").chars().take(40).collect::<String>();
                    undo.lock().unwrap().entry(session).or_default().push(GitUndo::Commit { sha: sha.clone(), what: format!("Commit “{subject}”") });
                    Ok(Response::Committed { sha })
                });
            }
            Request::GitUndo { session } => {
                let undo = self.git_undo.clone();
                return self.in_workdir(client, id, session, move |dir| async move {
                    let Some(last) = undo.lock().unwrap().get_mut(&session).and_then(Vec::pop) else {
                        return Err(RpcError::new(ErrorCode::Conflict, "there's nothing tessel did here to take back"));
                    };
                    let result = match &last {
                        GitUndo::Index { before, after, .. } => {
                            let paths: Vec<String> = after.iter().map(|(p, _)| p.clone()).collect();
                            if workspace::index_entries(&dir, &paths).await != *after {
                                Err(RpcError::new(ErrorCode::Conflict, "those files were staged differently since; nothing was undone"))
                            } else {
                                workspace::restore_entries(&dir, before).await
                            }
                        }
                        GitUndo::Commit { sha, .. } => workspace::uncommit(&dir, sha).await,
                    };
                    result?;
                    Ok(Response::WorkTree { tree: tree_with_undo(&dir, &undo, session).await })
                });
            }
            Request::GitSuggestMessage { session } => {
                let backend = self.backend.clone();
                return self.in_workdir(client, id, session, move |dir| async move {
                    let diff = workspace::staged_diff(&dir).await?;
                    let prompt = format!(
                        "Write the commit message for this staged change. Reply with the message only: one line, imperative, \
lowercase after a conventional prefix (feat:, fix: or chore:), under 60 characters, no quotes, no trailing period.\n\n{diff}"
                    );
                    let message = backend.one_shot(prompt, "haiku").await.map_err(|e| RpcError::new(ErrorCode::Internal, format!("couldn't write a message: {e:#}")))?;
                    let message = message.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim().trim_matches(['"', '`', '\'']).to_owned();
                    Ok(Response::Suggested { message })
                });
            }
            Request::FsList { session, path } => {
                return self.in_workdir(client, id, session, |dir| async move { workspace::list(&dir, &path).await.map(|entries| Response::Dir { entries }) });
            }
            Request::FsRead { session, path } => {
                return self.in_workdir(client, id, session, |dir| async move { workspace::read(&dir, &path).await.map(|file| Response::File { file }) });
            }
            Request::FsWrite { session, path, content, .. } => {
                return self.in_workdir(client, id, session, |dir| async move { workspace::write(&dir, &path, &content).await.map(|()| Response::Ack) });
            }
            Request::PrStatus { session } => {
                return self.in_workdir(client, id, session, |dir| async move { Ok(Response::Pr { status: workspace::pr_status(&dir).await }) });
            }
            Request::UiRegister { name } => {
                let seq = self.next_ui;
                self.next_ui += 1;
                if let Some(c) = self.clients.get_mut(&client) {
                    c.window = Some((name, seq));
                }
                let watched = self.clients.values().any(|c| c.ui_kinds.is_some());
                self.send(client, Envelope::Ev { ev: Event::UiWatched { watched } });
                Ok(Response::Ack)
            }
            Request::TermOpen { session, cwd, command, cols, rows } => {
                let session_cwd = session.and_then(|s| self.sessions.get(&s)).map(|l| l.state.meta.cwd.clone());
                if let Some(s) = session
                    && session_cwd.is_none()
                {
                    return Some(Err(not_found(s)));
                }
                let cwd = cwd.or(session_cwd).unwrap_or_else(|| std::env::var("HOME").unwrap_or_else(|_| "/".into()));
                if !std::path::Path::new(&cwd).is_dir() {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, format!("{cwd} is not a directory"))));
                }
                let id = self.next_term;
                self.next_term += 1;
                let info = tessel_proto::term::TermInfo { id, title: String::new(), cwd, cols: cols.max(2), rows: rows.max(1), session, command, exited: None };
                let mut env = vec![("TESSEL_HOME".to_owned(), self.cfg.paths.home.to_string_lossy().into_owned())];
                if let Some(s) = session {
                    env.push(("TESSEL_SESSION".into(), s.to_string()));
                }
                match crate::term::spawn(info.clone(), env, self.tx.clone()) {
                    Ok(handle) => {
                        self.terms.insert(id, handle);
                        self.tell_everyone(Event::TermUpdated { info: info.clone() });
                        Ok(Response::Term { info })
                    }
                    Err(e) => Err(RpcError::new(ErrorCode::Internal, format!("couldn't start a terminal: {e}"))),
                }
            }
            Request::TermList => Ok(Response::Terms { terms: self.terms.values().map(|t| t.info.clone()).collect() }),
            Request::TermAttach { term } => match self.terms.get(&term) {
                // answered with its dump, from its thread.
                Some(t) => {
                    t.send(crate::term::Cmd::Dump { client, id });
                    return None;
                }
                None => Err(no_term(term)),
            },
            Request::TermDetach { term } => {
                if let Some(t) = self.terms.get_mut(&term) {
                    t.attached.remove(&client);
                }
                Ok(Response::Ack)
            }
            Request::TermInput { term, text } => match self.terms.get(&term) {
                Some(t) if t.info.exited.is_some() => Err(RpcError::new(ErrorCode::Conflict, format!("terminal {term} has exited"))),
                Some(t) => {
                    t.send(crate::term::Cmd::Input(text.into_bytes()));
                    Ok(Response::Ack)
                }
                None => Err(no_term(term)),
            },
            Request::TermResize { term, cols, rows } => match self.terms.get_mut(&term) {
                Some(t) => {
                    let (cols, rows) = (cols.max(2), rows.max(1));
                    if (t.info.cols, t.info.rows) != (cols, rows) {
                        t.send(crate::term::Cmd::Resize(cols, rows));
                        (t.info.cols, t.info.rows) = (cols, rows);
                        let info = t.info.clone();
                        self.tell_everyone(Event::TermUpdated { info });
                    }
                    Ok(Response::Ack)
                }
                None => Err(no_term(term)),
            },
            Request::TermClose { term } => match self.terms.get_mut(&term) {
                Some(t) if t.info.exited.is_some() => {
                    self.forget_term(term);
                    Ok(Response::Ack)
                }
                Some(t) => {
                    t.closing = true;
                    t.send(crate::term::Cmd::Hangup);
                    Ok(Response::Ack)
                }
                None => Err(no_term(term)),
            },
            Request::TermCapture { term, history } => match self.terms.get(&term) {
                Some(t) => {
                    t.send(crate::term::Cmd::Capture { client, id, history });
                    return None;
                }
                None => Err(no_term(term)),
            },
            Request::TermWait { term, text, timeout_ms } => match self.terms.get(&term) {
                Some(t) if text.is_none() && t.info.exited.is_some() => Ok(Response::Term { info: t.info.clone() }),
                Some(t) => {
                    let deadline = std::time::Instant::now() + Duration::from_millis(timeout_ms.min(3_600_000));
                    t.send(crate::term::Cmd::Wait { client, id, text, deadline });
                    return None;
                }
                None => Err(no_term(term)),
            },
            Request::Settings => Ok(Response::Settings { settings: self.settings.clone() }),
            Request::SetSettings { patch } => {
                let Some(patch) = patch.as_object() else {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, "settings change with an object, like {\"agent_tools\": false}")));
                };
                let mut all = serde_json::to_value(&self.settings).unwrap_or_default();
                for (k, v) in patch {
                    if all.get(k).is_none() {
                        let known: Vec<String> = all.as_object().map(|o| o.keys().cloned().collect()).unwrap_or_default();
                        return Some(Err(RpcError::new(ErrorCode::Invalid, format!("there's no setting \"{k}\". settings: {}", known.join(", ")))));
                    }
                    all[k] = v.clone();
                }
                let next: tessel_proto::DaemonSettings = match serde_json::from_value(all) {
                    Ok(n) => n,
                    Err(e) => return Some(Err(RpcError::new(ErrorCode::Invalid, format!("that isn't a setting's value: {e}")))),
                };
                if next.models.len() > tessel_proto::MAX_FAVORITES {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, format!("at most {} models are favourites", tessel_proto::MAX_FAVORITES))));
                }
                if let Some(bad) = next.models.iter().filter_map(|m| m.effort.as_deref()).find(|e| !tessel_proto::EFFORTS.contains(e)) {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, format!("\"{bad}\" isn't an effort: {}", tessel_proto::EFFORTS.join(", ")))));
                }
                if next != self.settings {
                    if let Err(e) = save_settings(&self.cfg.paths.settings(), &next) {
                        return Some(Err(RpcError::new(ErrorCode::Internal, format!("couldn't save the settings: {e}"))));
                    }
                    self.settings = next;
                    self.tell_everyone(Event::Settings { settings: self.settings.clone() });
                }
                Ok(Response::Settings { settings: self.settings.clone() })
            }
            Request::UiWindows => {
                let mut windows: Vec<(u64, String)> = self.clients.values().filter_map(|c| c.window.as_ref().map(|(n, s)| (*s, n.clone()))).collect();
                windows.sort();
                Ok(Response::UiWindows { windows: windows.into_iter().map(|(_, n)| n).collect() })
            }
            Request::UiExec { window, cmd, args } => {
                // by name, else the window that registered last: the one most likely in front.
                let target = self
                    .clients
                    .iter()
                    .filter_map(|(id, c)| c.window.as_ref().map(|(n, s)| (*id, n, *s)))
                    .filter(|(_, n, _)| window.as_ref().is_none_or(|w| w == *n))
                    .max_by_key(|(_, _, s)| *s)
                    .map(|(id, _, _)| id);
                let Some(target) = target else {
                    return Some(Err(RpcError::new(
                        ErrorCode::NotFound,
                        match window {
                            Some(w) => format!("no tessel window called {w} is open"),
                            None => "no tessel window is open. start tessel, then try again".into(),
                        },
                    )));
                };
                let call = self.next_ui;
                self.next_ui += 1;
                self.ui_calls.insert(call, UiCall { client, id, window: target, deadline: now_ms() + UI_CALL_TIMEOUT_MS });
                self.respond_event(target, Event::UiCall { call, cmd, args });
                return None;
            }
            Request::UiAnswer { call, ok, err } => {
                if let Some(c) = self.ui_calls.remove(&call).filter(|c| c.window == client) {
                    let result = match err {
                        Some(e) => Err(RpcError::new(ErrorCode::Invalid, e)),
                        None => Ok(Response::UiResult { value: ok.unwrap_or(serde_json::Value::Null) }),
                    };
                    self.respond(c.client, c.id, result);
                }
                Ok(Response::Ack)
            }
            Request::UiPublish { kind, data } => {
                let Some(name) = self.clients.get(&client).and_then(|c| c.window.as_ref()).map(|(n, _)| n.clone()) else {
                    return Some(Err(RpcError::new(ErrorCode::Invalid, "only a window can publish; register first")));
                };
                let targets: Vec<ClientId> = self
                    .clients
                    .iter()
                    .filter(|(_, c)| c.ready && c.ui_kinds.as_ref().is_some_and(|k| k.is_empty() || k.contains(&kind)))
                    .map(|(id, _)| *id)
                    .collect();
                for t in targets {
                    self.send(t, Envelope::Ev { ev: Event::Ui { window: name.clone(), kind: kind.clone(), data: data.clone() } });
                }
                Ok(Response::Ack)
            }
            Request::UiSubscribe { kinds } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.ui_kinds = Some(kinds);
                }
                self.tell_windows_if_watched();
                Ok(Response::Ack)
            }
            Request::SurfaceOp { session, op } => {
                use tessel_proto::ItemBody;
                use tessel_proto::surface::{MAX_SURFACE_BYTES, SurfaceOp};
                let Some(live) = self.sessions.get(&session) else { return Some(Err(not_found(session))) };
                let item = ItemId::from(format!("surface/{}", op.surface()).as_str());
                let existing = live.state.item(&item).map(|i| &i.body);
                let problem = match &op {
                    SurfaceOp::Put { node, region, .. } => {
                        if serde_json::to_vec(node).map_or(true, |b| b.len() > MAX_SURFACE_BYTES) {
                            Some(format!("a surface can be at most {} KB", MAX_SURFACE_BYTES / 1024))
                        } else if !node.is_object() {
                            Some("a surface's node is an object like {\"k\": \"text\", \"p\": {\"text\": \"hi\"}}".into())
                        } else if let Some(first) = tessel_proto::surface::validate(node).into_iter().next() {
                            Some(first)
                        } else if region != "inline" && region != "pane" {
                            Some(format!("\"{region}\" isn't a region: use inline or pane"))
                        } else {
                            None
                        }
                    }
                    SurfaceOp::Set { surface, element, .. } => match existing {
                        Some(ItemBody::Surface { node, .. }) => {
                            let mut probe = node.clone();
                            (!tessel_proto::surface::set_props(&mut probe, element, &serde_json::json!({}))).then(|| {
                                let known = tessel_proto::surface::ids(node);
                                let known = if known.is_empty() { "it has no ids".to_owned() } else { format!("its ids: {}", known.join(", ")) };
                                format!("surface {surface} has no element \"{element}\" ({known}; leave element out to change the top one)")
                            })
                        }
                        _ => Some(format!("there's no surface \"{surface}\"; put it first")),
                    },
                    SurfaceOp::Remove { surface } => (!matches!(existing, Some(ItemBody::Surface { .. }))).then(|| format!("there's no surface \"{surface}\"")),
                };
                match problem {
                    Some(p) => Err(RpcError::new(ErrorCode::Invalid, p)),
                    None => self.apply(session, Input::Surface(op)).map(|_| Response::Ack),
                }
            }
            Request::SurfaceEvent { session, surface, element, act, value } => {
                // the agent that drew it hears about it, as a message in its own session.
                let text = tessel_proto::surface::event_message(&surface, &element, &act, &value);
                return self.request(client, id, Request::SessionSend { cmd_id: Uuid::new_v4(), session, text, attachments: Vec::new() });
            }
            Request::Shutdown => Ok(Response::Ack),
            Request::Unknown => Err(RpcError::new(ErrorCode::Invalid, "this daemon does not know that request")),
        })
    }

    /// run `work` in the session's working directory, off the main loop, and
    /// send its answer when it is done. `None` is "the answer comes later".
    fn in_workdir<F, Fut>(&self, client: ClientId, id: u64, session: SessionId, work: F) -> Option<Result<Response, RpcError>>
    where
        F: FnOnce(std::path::PathBuf) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = Result<Response, RpcError>> + Send + 'static,
    {
        let Some(live) = self.sessions.get(&session) else { return Some(Err(not_found(session))) };
        let dir = std::path::PathBuf::from(&live.state.meta.cwd);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = work(dir).await;
            let _ = tx.send(HubMsg::Reply { client, id, result });
        });
        None
    }

    fn make_worktree(&self, client: ClientId, id: u64, create: Create, spec: WorktreeSpec) {
        let tx = self.tx.clone();
        let root = self.cfg.paths.workspaces.clone();
        let hint = create.goal.clone().or_else(|| create.prompt.clone()).unwrap_or_default();
        tokio::spawn(async move {
            let result = worktree::create(&create.cwd, &spec, &root, &hint).await;
            let _ = tx.send(HubMsg::WorktreeReady { client, id, create: Box::new(create), result });
        });
    }

    fn create_session(&mut self, c: Create, cwd: String, wt: Option<WorktreeInfo>) -> Result<Response, RpcError> {
        let now = now_ms();
        let id = SessionId::new();
        let goal = c
            .goal
            .filter(|g| !g.trim().is_empty())
            .or_else(|| c.prompt.as_deref().map(crate::activity::truncate).filter(|g| !g.is_empty()))
            .unwrap_or_else(|| "untitled".to_owned());
        let meta = Meta { id, goal, cwd, worktree: wt, parent: c.parent, created_ms: now, linked_dirs: Vec::new() };
        let mut state = SessionState::new(meta);
        state.model = c.model;
        let mut fork_from = None;
        if let Some(f) = c.fork {
            state.effort = f.effort;
            state.fast = f.fast;
            state.permission_mode = f.permission_mode;
            state.seed_items(f.items);
            // in another folder the agent looks for its conversations somewhere else: put a copy there.
            if f.from_cwd != state.meta.cwd
                && let Err(e) = copy_conversation(&f.agent_id, &state.meta.cwd)
            {
                eprintln!("tesseld: forking {}: {e}", f.agent_id);
            }
            fork_from = Some(f.agent_id);
        }
        self.store.save(state.persisted(), state.items().to_vec());
        self.sessions.insert(
            id,
            Live { state, agent: None, generation: 0, last_summary: None, interrupt_deadline: None, restart_when_idle: false, fork_from },
        );
        self.publish_summary(id, now);
        if let Some(prompt) = c.prompt.filter(|p| !p.trim().is_empty()) {
            // the session exists either way; a failed first send is reported in it.
            if let Err(e) = self.send_user(id, c.cmd_id, prompt, Vec::new()) {
                eprintln!("tesseld: first message for {id}: {e}");
            }
        }
        let summary = self.sessions[&id].state.summary(now_ms());
        let response = Response::Session { summary };
        self.remember(c.cmd_id, response.clone());
        Ok(response)
    }

    /// change which folders a session may work in. a running agent only
    /// learns its folders when it starts, so it is restarted once idle.
    fn relink(&mut self, session: SessionId, input: Input) -> Result<Response, RpcError> {
        self.apply(session, input)?;
        if let Some(live) = self.sessions.get_mut(&session)
            && live.agent.is_some()
        {
            live.restart_when_idle = true;
        }
        Ok(Response::Ack)
    }

    fn send_user(&mut self, session: SessionId, cmd_id: Uuid, text: String, attachments: Vec<Attachment>) -> Result<(), RpcError> {
        if text.trim().is_empty() && attachments.is_empty() {
            return Err(RpcError::new(ErrorCode::Invalid, "empty message"));
        }
        // refuse what the agent can't read before anything is sent, so a
        // message never arrives missing the file it talks about.
        if let Some(bad) = attachments.iter().find(|a| !a.is_supported()) {
            let why = format!("{} can't be attached ({}). images and pdfs can; for anything else, mention its path", bad.name, bad.media_type);
            return Err(RpcError::new(ErrorCode::Invalid, why));
        }
        // a chat started empty (a new tab) is called what you first say to it.
        if self.sessions.get(&session).is_some_and(|l| l.state.meta.goal == "untitled") {
            let goal = crate::activity::truncate(text.trim());
            if !goal.is_empty() {
                self.apply(session, Input::SetGoal { goal })?;
            }
        }
        self.ensure_process(session)?;
        let live = self.sessions.get(&session).ok_or_else(|| not_found(session))?;
        if let Some(agent) = &live.agent {
            agent.send_user(&text, &attachments);
        }
        let names = attachments.into_iter().map(|a| a.name).collect();
        self.apply(session, Input::UserSent { item_id: ItemId(format!("user/{cmd_id}")), text, attachments: names })?;
        Ok(())
    }

    /// make sure an agent process exists for this session, starting (or
    /// resuming) one if not. reaping and crash recovery both lead here.
    fn ensure_process(&mut self, session: SessionId) -> Result<(), RpcError> {
        let live = self.sessions.get_mut(&session).ok_or_else(|| not_found(session))?;
        if live.agent.is_some() {
            return Ok(());
        }
        let cwd = live.state.meta.cwd.clone();
        if !Path::new(&cwd).is_dir() {
            return Err(RpcError::new(ErrorCode::Conflict, format!("{cwd} no longer exists")));
        }
        let known = live.state.agent_session_id.clone();
        // a fork's first start carries on from its parent's conversation under a new id.
        let fork = known.is_none().then(|| live.fork_from.clone()).flatten();
        let opts = SpawnOpts {
            cwd: cwd.into(),
            resume: known.is_some() || fork.is_some(),
            fork: fork.is_some(),
            agent_session_id: known.or(fork).unwrap_or_else(|| session.to_string()),
            model: live.state.model.clone(),
            permission_mode: live.state.permission_mode.clone(),
            effort: live.state.effort.clone(),
            fast: live.state.fast,
            add_dirs: live.state.meta.linked_dirs.iter().map(Into::into).collect(),
            raw_log: Some(self.cfg.paths.raw_log(&session)),
            tools: self.cfg.tools_bin.as_ref().filter(|_| self.settings.agent_tools).map(|bin| tessel_agent::backend::AgentTools {
                command: bin.clone(),
                env: vec![
                    ("TESSEL_SESSION".into(), session.to_string()),
                    ("TESSEL_HOME".into(), self.cfg.paths.home.to_string_lossy().into_owned()),
                ],
            }),
        };
        let spawned = self
            .backend
            .spawn(opts)
            .map_err(|e| RpcError::new(ErrorCode::Internal, format!("could not start the agent: {e:#}")))?;
        live.generation += 1;
        live.restart_when_idle = false;
        let generation = live.generation;
        if let Some(pid) = spawned.agent.pid() {
            crate::orphans::record(&self.cfg.paths.session_dir(&session), pid);
        }
        live.agent = Some(spawned.agent);
        let tx = self.tx.clone();
        let mut output = spawned.output;
        tokio::spawn(async move {
            while let Some(out) = output.recv().await {
                if tx.send(HubMsg::Agent { session, generation, output: out }).is_err() {
                    break;
                }
            }
        });
        self.apply(session, Input::Spawning)?;
        Ok(())
    }

    fn agent_output(&mut self, session: SessionId, generation: u64, output: AgentOutput) {
        let Some(live) = self.sessions.get_mut(&session) else { return };
        if live.generation != generation {
            return;
        }
        match output {
            AgentOutput::Alive => {
                let _ = self.apply(session, Input::Alive);
            }
            AgentOutput::Events(events) => {
                for ev in events {
                    let _ = self.apply(session, Input::Agent(ev));
                }
            }
            AgentOutput::Exited { code } => {
                live.agent = None;
                live.interrupt_deadline = None;
                crate::orphans::clear(&self.cfg.paths.session_dir(&session));
                let _ = self.apply(session, Input::Agent(AgentEvent::Exited { code }));
            }
        }
    }

    // ---- state changes -----------------------------------------------------

    fn apply(&mut self, session: SessionId, input: Input) -> Result<(), RpcError> {
        let now = now_ms();
        let live = self.sessions.get_mut(&session).ok_or_else(|| not_found(session))?;
        let fx = live.state.apply(input, now);
        if !live.state.turn_open() {
            live.interrupt_deadline = None;
        }
        self.after(session, fx, now);
        Ok(())
    }

    fn after(&mut self, session: SessionId, fx: Effects, now: Millis) {
        let Some(live) = self.sessions.get(&session) else { return };
        let persist = !fx.upserts.is_empty() || fx.pending_changed || fx.summary_changed;
        if persist {
            self.store.save(live.state.persisted(), fx.upserts.clone());
        }
        let pending = fx.pending_changed.then(|| live.state.pending().to_vec());
        let watching: Vec<ClientId> =
            self.clients.iter().filter(|(_, c)| c.detail.contains(&session)).map(|(id, _)| *id).collect();
        for client in watching {
            for item in &fx.upserts {
                self.send(client, Envelope::Ev { ev: Event::ItemUpserted { session, item: item.clone() } });
            }
            for d in &fx.deltas {
                let ev = Event::ItemDelta { session, item_id: d.item_id.clone(), offset: d.offset, text: d.text.clone() };
                self.send(client, Envelope::Ev { ev });
            }
            if let Some(pending) = &pending {
                self.send(client, Envelope::Ev { ev: Event::PendingChanged { session, pending: pending.clone() } });
            }
        }
        if persist {
            self.publish_summary(session, now);
        }
    }

    /// tell subscribers about a session's summary, but only if it changed.
    fn publish_summary(&mut self, session: SessionId, now: Millis) {
        let Some(live) = self.sessions.get_mut(&session) else { return };
        let summary = live.state.summary(now);
        if live.last_summary.as_ref() == Some(&summary) {
            return;
        }
        live.last_summary = Some(summary.clone());
        self.broadcast_summary_event(Event::SessionUpdated { summary }, Some(session));
    }

    fn broadcast_summary_event(&mut self, ev: Event, session: Option<SessionId>) {
        let targets: Vec<ClientId> = self
            .clients
            .iter()
            .filter(|(_, c)| c.ready && (c.summary || session.is_some_and(|s| c.detail.contains(&s))))
            .map(|(id, _)| *id)
            .collect();
        for client in targets {
            self.send(client, Envelope::Ev { ev: ev.clone() });
        }
    }

    fn summaries(&self) -> Vec<SessionSummary> {
        let now = now_ms();
        let mut all: Vec<_> = self.sessions.values().map(|l| l.state.summary(now)).collect();
        all.sort_by_key(|s| (s.created_ms, s.id));
        all
    }

    fn tick(&mut self) {
        let now = now_ms();
        let reap_after = self.cfg.idle_reap.as_millis() as Millis;
        let ids: Vec<SessionId> = self.sessions.keys().copied().collect();
        for id in ids {
            // time alone changes a summary: silence becomes "stalled".
            self.publish_summary(id, now);
            let Some(live) = self.sessions.get_mut(&id) else { continue };
            let Some(agent) = &live.agent else { continue };
            if live.interrupt_deadline.is_some_and(|d| now >= d) && live.state.turn_open() {
                // it was asked nicely and did not stop.
                live.interrupt_deadline = None;
                agent.kill();
                continue;
            }
            let s = &live.state;
            let idle = !s.turn_open() && s.pending().is_empty() && s.summary(now).background_tasks == 0;
            if idle && (live.restart_when_idle || now.saturating_sub(s.updated_ms) >= reap_after) {
                agent.kill();
            }
        }
        // a window that never answered: its caller is not left waiting forever.
        let late: Vec<u64> = self.ui_calls.iter().filter(|(_, c)| now >= c.deadline).map(|(k, _)| *k).collect();
        for call in late {
            if let Some(c) = self.ui_calls.remove(&call) {
                self.respond(c.client, c.id, Err(RpcError::new(ErrorCode::Internal, "the window didn't answer in time")));
            }
        }
        // a client we dropped events for is told so as soon as it has room.
        for c in self.clients.values_mut() {
            if c.lagged && c.tx.try_send(Envelope::Ev { ev: Event::Lagged }.into()).is_ok() {
                c.lagged = false;
            }
        }
    }

    // ---- talking to clients --------------------------------------------------

    fn respond(&mut self, client: ClientId, id: u64, result: Result<Response, RpcError>) {
        let env = match result {
            Ok(ok) => Envelope::Res { id, ok: Some(ok), err: None },
            Err(err) => Envelope::Res { id, ok: None, err: Some(err) },
        };
        let Some(c) = self.clients.get_mut(&client) else { return };
        // a response can't be dropped: the client is waiting on it. if there
        // is no room, the client is beyond saving; cut it loose to reconnect.
        if c.tx.try_send(env.into()).is_err() {
            c.dead = true;
        }
    }

    /// an event a client must not miss (a command for a window): sent like a response.
    fn respond_event(&mut self, client: ClientId, ev: Event) {
        let Some(c) = self.clients.get_mut(&client) else { return };
        if c.tx.try_send(Envelope::Ev { ev }.into()).is_err() {
            c.dead = true;
        }
    }

    /// windows build no events while nobody listens; tell them when that changes.
    fn tell_windows_if_watched(&mut self) {
        let watched = self.clients.values().any(|c| c.ui_kinds.is_some());
        let windows: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.window.is_some()).map(|(id, _)| *id).collect();
        for w in windows {
            self.send(w, Envelope::Ev { ev: Event::UiWatched { watched } });
        }
    }

    fn send(&mut self, client: ClientId, out: impl Into<Out>) {
        let Some(c) = self.clients.get_mut(&client) else { return };
        if c.lagged || c.dead {
            return;
        }
        match c.tx.try_send(out.into()) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                // events are droppable, but never silently: the client must
                // know its picture is stale. it resubscribes for a fresh one,
                // and attaches to its terminals again (each starts from a dump).
                c.lagged = true;
                c.summary = false;
                c.detail.clear();
                for t in self.terms.values_mut() {
                    t.attached.remove(&client);
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => c.dead = true,
        }
    }

    /// something every connected client hears.
    fn tell_everyone(&mut self, ev: Event) {
        let everyone: Vec<ClientId> = self.clients.iter().filter(|(_, c)| c.ready).map(|(id, _)| *id).collect();
        for c in everyone {
            self.send(c, Envelope::Ev { ev: ev.clone() });
        }
    }

    fn forget_term(&mut self, term: TermId) {
        if let Some(t) = self.terms.remove(&term) {
            t.send(crate::term::Cmd::Quit);
            self.tell_everyone(Event::TermRemoved { term });
        }
    }

    fn remember(&mut self, cmd_id: Uuid, response: Response) {
        if self.done.insert(cmd_id, response).is_none() {
            self.done_order.push_back(cmd_id);
            if self.done_order.len() > REMEMBERED_COMMANDS
                && let Some(old) = self.done_order.pop_front()
            {
                self.done.remove(&old);
            }
        }
    }
}

fn no_term(term: TermId) -> RpcError {
    RpcError::new(ErrorCode::NotFound, format!("no terminal {term}"))
}

fn not_found(session: SessionId) -> RpcError {
    RpcError::new(ErrorCode::NotFound, format!("no session {session}"))
}

/// the settings file, or the defaults when there is none or it can't be read.
fn load_settings(path: &std::path::Path) -> tessel_proto::DaemonSettings {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            eprintln!("tesseld: {} isn't readable ({e}); using the defaults", path.display());
            tessel_proto::DaemonSettings::default()
        }),
        Err(_) => tessel_proto::DaemonSettings::default(),
    }
}

/// written whole, then moved into place: a crash never leaves half a file.
fn save_settings(path: &std::path::Path, s: &tessel_proto::DaemonSettings) -> std::io::Result<()> {
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s).unwrap_or_default())?;
    std::fs::rename(tmp, path)
}

/// the folder claude keeps a directory's conversations in: its path, resolved,
/// with everything but letters and digits made a dash.
fn conversations_dir(cwd: &str) -> std::path::PathBuf {
    let base = std::env::var_os("CLAUDE_CONFIG_DIR").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".claude"));
    let real = std::fs::canonicalize(cwd).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| cwd.to_owned());
    let name: String = real.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    base.join("projects").join(name)
}

/// put a copy of a conversation where an agent started in `to_cwd` will look for it.
fn copy_conversation(agent_id: &str, to_cwd: &str) -> std::io::Result<()> {
    let projects = conversations_dir(to_cwd).parent().map(std::path::Path::to_path_buf).unwrap_or_default();
    let file = format!("{agent_id}.jsonl");
    let from = std::fs::read_dir(&projects)?.flatten().map(|d| d.path().join(&file)).find(|p| p.is_file()).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, format!("no conversation {agent_id}")))?;
    let dir = conversations_dir(to_cwd);
    std::fs::create_dir_all(&dir)?;
    std::fs::copy(from, dir.join(file)).map(drop)
}

#[cfg(test)]
mod fork_tests {
    #[test]
    fn a_folder_s_conversations_are_kept_under_its_resolved_path_with_dashes() {
        let d = super::conversations_dir("/no/such/place.v2");
        assert!(d.ends_with("projects/-no-such-place-v2"), "{d:?}");
    }
}
