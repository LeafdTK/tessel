//! the connection to the daemon, on its own thread.
//!
//! the ui thread never waits on the socket. it drops commands into a channel
//! and receives `UiEvent`s from another. if the daemon goes away this keeps
//! reconnecting (starting one if needed) and re-subscribes to whatever was open.

use tessel_client::{Client, ClientError, Events};
use tessel_proto::term::{TermId, TermInfo};
use tessel_proto::{
    Attachment, DirEntry, Event, FileContent, FileDiff, GitSummary, Item, ItemId, ModelInfo, PermissionDecision, PermissionRequest,
    PrStatus, Request, RequestId, Response, Seq, SessionId, SessionSnapshot, SessionSummary, WorktreeSpec,
};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug)]
pub enum Cmd {
    /// the chat in the focused pane (or none).
    Open(Option<SessionId>),
    /// the chats the other panes show, which the window follows as well.
    Follow(Vec<SessionId>),
    /// do something to a session's page, from the pane.
    Browser { session: SessionId, act: tessel_proto::BrowserAct },
    /// this window shows pages itself (see `hosts_pages`).
    BrowserHost,
    Create { cwd: String, worktree: bool, prompt: String },
    /// carry a session's conversation on in a new tab, or a new workspace, and open it.
    Fork { session: SessionId, workspace: bool },
    Send { session: SessionId, text: String, attachments: Vec<Attachment> },
    SetModel { session: SessionId, model: String },
    SetEffort { session: SessionId, effort: Option<String> },
    SetFast { session: SessionId, fast: bool },
    SetMode { session: SessionId, mode: String },
    Models,
    RefreshMcp(SessionId),
    LinkDir { session: SessionId, path: String },
    UnlinkDir { session: SessionId, path: String },
    // the session's working directory. each is answered with the matching `UiEvent`.
    Git(SessionId),
    Diff { session: SessionId, path: String },
    List { session: SessionId, path: String },
    Read { session: SessionId, path: String },
    Write { session: SessionId, path: String, content: String },
    Pr(SessionId),
    Answer { session: SessionId, request_id: RequestId, decision: PermissionDecision },
    Interrupt(SessionId),
    MarkSeen { session: SessionId, seq: Seq },
    Delete { session: SessionId, remove_worktree: bool },
    /// rename: the one-line "what is this".
    SetGoal { session: SessionId, goal: String },
    /// the answer to a window command a script sent.
    UiAnswer { call: u64, result: Result<serde_json::Value, String> },
    /// something happened in the window, for whoever listens.
    UiPublish { kind: String, data: serde_json::Value },
    /// someone acted on a surface an agent drew.
    SurfaceEvent { session: SessionId, surface: String, element: String, act: String, value: serde_json::Value },
    /// change some of the daemon's settings (a partial object).
    SetSettings(serde_json::Value),
    /// start a terminal for a pane.
    TermOpen { pane: String, session: Option<SessionId>, cwd: Option<String>, command: Vec<String>, cols: u16, rows: u16 },
    TermAttach(TermId),
    /// keys, as the terminal encoded them: sent as a binary frame.
    TermInput(TermId, Vec<u8>),
    TermResize(TermId, u16, u16),
    TermClose(TermId),
    /// any request, answered as `UiEvent::Answer` with the same tag: for
    /// modules that just ask and show (the git graph).
    Ask { tag: &'static str, req: Request },
}

#[derive(Debug)]
pub enum UiEvent {
    Connected,
    Down(String),
    Sessions(Vec<SessionSummary>),
    Session(SessionSummary),
    Removed(SessionId),
    Snapshot(Box<SessionSnapshot>),
    Item { session: SessionId, item: Item },
    Delta { session: SessionId, item_id: ItemId, offset: u64, text: String },
    Pending { session: SessionId, pending: Vec<PermissionRequest> },
    /// the models the agent offers, and its slash commands.
    Models(Vec<ModelInfo>, Vec<tessel_proto::SlashCommand>),
    Git { session: SessionId, summary: GitSummary },
    Diff { session: SessionId, diff: FileDiff },
    Dir { session: SessionId, path: String, entries: Vec<DirEntry> },
    File { session: SessionId, file: FileContent },
    Saved { session: SessionId, path: String },
    Pr { session: SessionId, status: PrStatus },
    /// a session we asked for was created and is now the open one.
    Created,
    /// a script wants a window command run.
    Call { call: u64, cmd: String, args: serde_json::Value },
    /// whether anyone listens to what the window publishes.
    Watched(bool),
    /// what the daemon is set to (on connecting, and whenever it changes).
    DaemonSettings(tessel_proto::DaemonSettings),
    /// every terminal (on connecting, and after falling behind): anything
    /// attached starts over from a fresh dump.
    Terms(Vec<TermInfo>),
    TermOpened { pane: String, session: Option<SessionId>, info: TermInfo },
    TermOutput(TermId, Vec<u8>),
    TermUpdated(TermInfo),
    TermRemoved(TermId),
    /// a frame of a session's page, decoded: bgra pixels.
    BrowserFrame { session: SessionId, bgra: Vec<u8>, width: u32, height: u32 },
    BrowserState { session: SessionId, state: tessel_proto::BrowserState },
    BrowserPicked { session: SessionId, picked: tessel_proto::Picked },
    BrowserActivity { session: SessionId, what: String, at: Option<(f64, f64)> },
    BrowserEngine(tessel_proto::BrowserEngine),
    /// do this to a session's page and answer `call` (the window shows pages itself).
    BrowserCall { call: u64, session: SessionId, act: tessel_proto::BrowserAct, agent: bool, first: String },
    /// the account's usage limits.
    Usage(Vec<tessel_proto::UsageWindow>),
    Answer { tag: &'static str, result: Result<Response, String> },
    /// something the user did failed. shown, not fatal.
    Error(String),
}

pub struct Bridge {
    pub cmds: mpsc::UnboundedSender<Cmd>,
}

pub fn start(home: PathBuf) -> (Bridge, mpsc::UnboundedReceiver<UiEvent>) {
    let (cmd_tx, cmd_rx) = mpsc::unbounded_channel();
    let (ui_tx, ui_rx) = mpsc::unbounded_channel();
    std::thread::Builder::new()
        .name("tessel-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(run(home, cmd_rx, ui_tx));
        })
        .expect("bridge thread");
    (Bridge { cmds: cmd_tx }, ui_rx)
}

/// the chats the window follows in detail: the focused one, and the rest on screen.
#[derive(Default)]
struct Following {
    open: Option<SessionId>,
    extra: Vec<SessionId>,
    /// the daemon can follow several (protocol 0.13). an older one follows
    /// only the open chat; the others then wait for a newer daemon.
    many: bool,
}

impl Following {
    fn all(&self) -> Vec<SessionId> {
        let mut all: Vec<SessionId> = self.open.into_iter().collect();
        all.extend(self.extra.iter().copied().filter(|s| Some(*s) != self.open));
        all
    }
}

async fn run(home: PathBuf, mut cmds: mpsc::UnboundedReceiver<Cmd>, ui: mpsc::UnboundedSender<UiEvent>) {
    let mut open = Following::default();
    let mut backoff = Duration::from_millis(100);
    loop {
        match tessel_client::connect_or_start(&home, "tessel").await {
            Err(e) => {
                if ui.send(UiEvent::Down(e.to_string())).is_err() {
                    return;
                }
                // a wrong-version daemon won't fix itself quickly; don't spin on it.
                let wait = if e.is_version_mismatch() { Duration::from_secs(5) } else { backoff };
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(Duration::from_secs(3));
            }
            Ok((client, events, welcome)) => {
                let client = Arc::new(client);
                // the daemon on disk is newer than the one running: it takes over,
                // with every terminal and agent carrying on, once no turn is mid-way.
                let bin = tessel_client::daemon_binary();
                let newer = bin.is_absolute()
                    && welcome.capabilities.iter().any(|c| c == "handoff")
                    && tessel_proto::local::binary_stamp(&bin).is_some_and(|s| tessel_proto::local::build_stamp(&welcome.build) != Some(s.as_str()));
                if newer && !ASKED_RESTART.swap(true, std::sync::atomic::Ordering::AcqRel) {
                    let c = client.clone();
                    tokio::spawn(async move {
                        match c.request(Request::DaemonRestart { now: false, wait_secs: None, bin: Some(bin.to_string_lossy().into_owned()) }).await {
                            Ok(r) => eprintln!("tessel: the daemon was updated: {r:?}"),
                            Err(e) => eprintln!("tessel: couldn't update the daemon: {e}"),
                        }
                    });
                }
                open.many = welcome.proto_minor >= 13;
                backoff = Duration::from_millis(100);
                if ui.send(UiEvent::Connected).is_err() {
                    return;
                }
                match session(&client, events, &mut cmds, &ui, &mut open).await {
                    Ended::UiGone => return,
                    Ended::Lost => {
                        if ui.send(UiEvent::Down("lost the connection to the daemon".into())).is_err() {
                            return;
                        }
                    }
                }
            }
        }
    }
}

enum Ended {
    UiGone,
    Lost,
}

/// the whole picture: every session, then each followed chat.
async fn subscribe(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>, f: &mut Following) -> Result<(), ClientError> {
    if !f.many {
        return subscribe_one(client, ui, &mut f.open).await;
    }
    if let Response::Subscribed { sessions: Some(sessions), .. } = client.request(Request::Subscribe { summary: true, detail: None, tail: 0 }).await? {
        let _ = ui.send(UiEvent::Sessions(sessions));
    }
    follow(client, ui, f).await
}

/// follow exactly what the window shows. snapshots come back only for chats
/// not followed already, so adding a pane doesn't resend the others.
async fn follow(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>, f: &mut Following) -> Result<(), ClientError> {
    if !f.many {
        return subscribe_one(client, ui, &mut f.open).await;
    }
    if let Response::Followed { snapshots, missing } = client.request(Request::Follow { sessions: f.all(), tail: 400 }).await? {
        // the session we had open was deleted while we were away.
        if f.open.is_some_and(|o| missing.contains(&o)) {
            f.open = None;
        }
        f.extra.retain(|s| !missing.contains(s));
        for snap in snapshots {
            let _ = ui.send(UiEvent::Snapshot(Box::new(snap)));
        }
    }
    Ok(())
}

/// an older daemon: one chat in detail, with the session list.
async fn subscribe_one(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>, open: &mut Option<SessionId>) -> Result<(), ClientError> {
    let reply = client.request(Request::Subscribe { summary: true, detail: *open, tail: 400 }).await;
    let reply = match reply {
        // the session we had open was deleted while we were away.
        Err(ClientError::Rpc(_)) if open.is_some() => {
            *open = None;
            client.request(Request::Subscribe { summary: true, detail: None, tail: 0 }).await?
        }
        other => other?,
    };
    if let Response::Subscribed { sessions, detail } = reply {
        if let Some(sessions) = sessions {
            let _ = ui.send(UiEvent::Sessions(sessions));
        }
        if let Some(snap) = detail {
            let _ = ui.send(UiEvent::Snapshot(Box::new(snap)));
        }
    }
    Ok(())
}

/// every terminal, so the window can (re)attach what it shows. an older
/// daemon has none; that's no error.
fn list_terms(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>) {
    let (client, ui) = (client.clone(), ui.clone());
    tokio::spawn(async move {
        if let Ok(Response::Terms { terms }) = client.request(Request::TermList).await {
            let _ = ui.send(UiEvent::Terms(terms));
        }
    });
}

/// ask about a session's folder without holding up everything else: git and
/// the disk can take a while, and tokens must keep streaming meanwhile.
fn ask(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>, req: Request, answer: impl FnOnce(Response) -> Option<UiEvent> + Send + 'static) {
    let (client, ui) = (client.clone(), ui.clone());
    tokio::spawn(async move {
        let ev = match client.request(req).await {
            Ok(r) => answer(r),
            Err(ClientError::Rpc(e)) => Some(UiEvent::Error(e.message)),
            // the connection went; the main loop notices and reconnects.
            Err(_) => None,
        };
        if let Some(ev) = ev {
            let _ = ui.send(ev);
        }
    });
}

async fn session(
    client: &Arc<Client>,
    mut events: Events,
    cmds: &mut mpsc::UnboundedReceiver<Cmd>,
    ui: &mpsc::UnboundedSender<UiEvent>,
    open: &mut Following,
) -> Ended {
    if subscribe(client, ui, open).await.is_err() {
        return Ended::Lost;
    }
    // this connection is the window: scripts and agents reach it through the daemon.
    // an older daemon doesn't know how; the window works anyway, unscripted.
    match client.request(Request::UiRegister { name: "main".into() }).await {
        Ok(_) | Err(ClientError::Rpc(_)) => {}
        Err(_) => return Ended::Lost,
    }
    // a window that shows pages itself says so again on every connection.
    if HOSTS_PAGES.load(std::sync::atomic::Ordering::Acquire) {
        match client.request(Request::BrowserHost { on: true }).await {
            Ok(_) | Err(ClientError::Rpc(_)) => {}
            Err(_) => return Ended::Lost,
        }
    }
    // the daemon's settings, for the settings page. an older daemon has none.
    ask(client, ui, Request::Settings, |r| match r {
        Response::Settings { settings } => Some(UiEvent::DaemonSettings(settings)),
        _ => None,
    });
    // one frame of a page decoding at a time.
    let decoding = Arc::new(std::sync::atomic::AtomicBool::new(false));
    list_terms(client, ui);
    loop {
        tokio::select! {
            ev = events.recv() => {
                let Some(ev) = ev else { return Ended::Lost };
                let out = match ev {
                    Event::SessionUpdated { summary } => UiEvent::Session(summary),
                    Event::SessionRemoved { session } => UiEvent::Removed(session),
                    Event::ItemUpserted { session, item } => UiEvent::Item { session, item },
                    Event::ItemDelta { session, item_id, offset, text } => UiEvent::Delta { session, item_id, offset, text },
                    Event::PendingChanged { session, pending } => UiEvent::Pending { session, pending },
                    Event::Lagged => {
                        // we fell behind and the daemon dropped events: ask for a fresh picture.
                        if subscribe(client, ui, open).await.is_err() {
                            return Ended::Lost;
                        }
                        // terminals too: the daemon stopped sending their bytes.
                        list_terms(client, ui);
                        continue;
                    }
                    Event::TermOutput { term, bytes } => UiEvent::TermOutput(term, bytes),
                    Event::TermUpdated { info } => UiEvent::TermUpdated(info),
                    Event::TermRemoved { term } => UiEvent::TermRemoved(term),
                    Event::BrowserFrame { session, jpeg, .. } => {
                        // decoded off this loop, and only one at a time: a frame that
                        // arrives mid-decode is dropped, so the pane never falls behind.
                        if !decoding.swap(true, std::sync::atomic::Ordering::AcqRel) {
                            let (ui, busy) = (ui.clone(), decoding.clone());
                            tokio::task::spawn_blocking(move || {
                                if let Some((bgra, width, height)) = decode_frame(&jpeg) {
                                    let _ = ui.send(UiEvent::BrowserFrame { session, bgra, width, height });
                                }
                                busy.store(false, std::sync::atomic::Ordering::Release);
                            });
                        }
                        continue;
                    }
                    Event::BrowserState { session, state } => UiEvent::BrowserState { session, state },
                    Event::BrowserPicked { session, picked } => UiEvent::BrowserPicked { session, picked },
                    Event::BrowserActivity { session, what, x, y } => UiEvent::BrowserActivity { session, what, at: x.zip(y) },
                    Event::BrowserEngine { engine } => UiEvent::BrowserEngine(engine),
                    Event::BrowserCall { call, session, act, agent, first } => UiEvent::BrowserCall { call, session, act, agent, first },
                    Event::Usage { windows } => UiEvent::Usage(windows),
                    Event::UiCall { call, cmd, args } => UiEvent::Call { call, cmd, args },
                    Event::UiWatched { watched } => UiEvent::Watched(watched),
                    Event::Settings { settings } => UiEvent::DaemonSettings(settings),
                    Event::Ui { .. } | Event::Unknown => continue,
                };
                if ui.send(out).is_err() {
                    return Ended::UiGone;
                }
            }
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { return Ended::UiGone };
                let result = match cmd {
                    Cmd::Open(id) => {
                        open.open = id;
                        follow(client, ui, open).await
                    }
                    Cmd::Browser { session, act } => {
                        ask(client, ui, Request::Browser { session, act, agent: false }, move |r| match r {
                            Response::Browser { state, .. } => Some(UiEvent::BrowserState { session, state }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::BrowserHost => {
                        HOSTS_PAGES.store(true, std::sync::atomic::Ordering::Release);
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _ = client.request(Request::BrowserHost { on: true }).await;
                        });
                        Ok(())
                    }
                    Cmd::Follow(extra) => {
                        open.extra = extra;
                        follow(client, ui, open).await
                    }
                    Cmd::Create { cwd, worktree, prompt } => {
                        let req = Request::SessionCreate {
                            cmd_id: Uuid::new_v4(),
                            cwd,
                            worktree: worktree.then(WorktreeSpec::default),
                            goal: None,
                            prompt: (!prompt.trim().is_empty()).then_some(prompt),
                            model: None,
                            parent: None,
                        };
                        match client.request(req).await {
                            Ok(Response::Session { summary }) => {
                                let id = summary.id;
                                let _ = ui.send(UiEvent::Session(summary));
                                let _ = ui.send(UiEvent::Created);
                                open.open = Some(id);
                                follow(client, ui, open).await
                            }
                            Ok(_) => Ok(()),
                            Err(e) => Err(e),
                        }
                    }
                    Cmd::Fork { session, workspace } => match client.request(Request::SessionFork { cmd_id: Uuid::new_v4(), session, workspace }).await {
                        Ok(Response::Session { summary }) => {
                            let id = summary.id;
                            let _ = ui.send(UiEvent::Session(summary));
                            let _ = ui.send(UiEvent::Created);
                            open.open = Some(id);
                            follow(client, ui, open).await
                        }
                        Ok(_) => Ok(()),
                        Err(e) => Err(e),
                    },
                    Cmd::Send { session, text, attachments } => client.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session, text, attachments }).await.map(drop),
                    Cmd::SetModel { session, model } => client.request(Request::SessionSetModel { cmd_id: Uuid::new_v4(), session, model }).await.map(drop),
                    Cmd::SetEffort { session, effort } => client.request(Request::SessionSetEffort { cmd_id: Uuid::new_v4(), session, effort }).await.map(drop),
                    Cmd::SetFast { session, fast } => client.request(Request::SessionSetFast { cmd_id: Uuid::new_v4(), session, fast }).await.map(drop),
                    Cmd::UiAnswer { call, result } => {
                        let (ok, err) = match result {
                            Ok(v) => (Some(v), None),
                            Err(e) => (None, Some(e)),
                        };
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _ = client.request(Request::UiAnswer { call, ok, err }).await;
                        });
                        Ok(())
                    }
                    Cmd::UiPublish { kind, data } => {
                        let client = client.clone();
                        tokio::spawn(async move {
                            let _ = client.request(Request::UiPublish { kind, data }).await;
                        });
                        Ok(())
                    }
                    Cmd::TermOpen { pane, session, cwd, command, cols, rows } => {
                        ask(client, ui, Request::TermOpen { session, cwd, command, cols, rows }, move |r| match r {
                            Response::Term { info } => Some(UiEvent::TermOpened { pane, session, info }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Ask { tag, req } => {
                        let (client, ui) = (client.clone(), ui.clone());
                        tokio::spawn(async move {
                            let result = match client.request(req).await {
                                Ok(r) => Ok(r),
                                Err(ClientError::Rpc(e)) => Err(e.message),
                                Err(e) => Err(e.to_string()),
                            };
                            let _ = ui.send(UiEvent::Answer { tag, result });
                        });
                        Ok(())
                    }
                    Cmd::TermAttach(term) => {
                        ask(client, ui, Request::TermAttach { term }, |_| None);
                        Ok(())
                    }
                    Cmd::TermInput(term, bytes) => client.send_term_input(term, &bytes),
                    Cmd::TermResize(term, cols, rows) => {
                        ask(client, ui, Request::TermResize { term, cols, rows }, |_| None);
                        Ok(())
                    }
                    Cmd::TermClose(term) => {
                        ask(client, ui, Request::TermClose { term }, |_| None);
                        Ok(())
                    }
                    Cmd::SetSettings(patch) => {
                        ask(client, ui, Request::SetSettings { patch }, |r| match r {
                            Response::Settings { settings } => Some(UiEvent::DaemonSettings(settings)),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::SurfaceEvent { session, surface, element, act, value } => {
                        client.request(Request::SurfaceEvent { session, surface, element, act, value }).await.map(drop)
                    }
                    Cmd::SetGoal { session, goal } => client.request(Request::SessionSetGoal { cmd_id: Uuid::new_v4(), session, goal }).await.map(drop),
                    Cmd::SetMode { session, mode } => client.request(Request::SessionSetMode { cmd_id: Uuid::new_v4(), session, mode }).await.map(drop),
                    Cmd::RefreshMcp(session) => client.request(Request::SessionRefreshMcp { session }).await.map(drop),
                    Cmd::LinkDir { session, path } => client.request(Request::SessionLinkDir { cmd_id: Uuid::new_v4(), session, path }).await.map(drop),
                    Cmd::UnlinkDir { session, path } => client.request(Request::SessionUnlinkDir { cmd_id: Uuid::new_v4(), session, path }).await.map(drop),
                    Cmd::Models => {
                        ask(client, ui, Request::ModelList, |r| match r {
                            Response::Models { models, commands } => Some(UiEvent::Models(models, commands)),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Git(session) => {
                        ask(client, ui, Request::GitStatus { session }, move |r| match r {
                            Response::Git { summary } => Some(UiEvent::Git { session, summary }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Diff { session, path } => {
                        ask(client, ui, Request::GitDiff { session, path, at: None, index: None }, move |r| match r {
                            Response::Diff { diff } => Some(UiEvent::Diff { session, diff }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::List { session, path } => {
                        let at = path.clone();
                        ask(client, ui, Request::FsList { session, path }, move |r| match r {
                            Response::Dir { entries } => Some(UiEvent::Dir { session, path: at, entries }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Read { session, path } => {
                        ask(client, ui, Request::FsRead { session, path }, move |r| match r {
                            Response::File { file } => Some(UiEvent::File { session, file }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Write { session, path, content } => {
                        let at = path.clone();
                        ask(client, ui, Request::FsWrite { cmd_id: Uuid::new_v4(), session, path, content }, move |r| match r {
                            Response::Ack => Some(UiEvent::Saved { session, path: at }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Pr(session) => {
                        ask(client, ui, Request::PrStatus { session }, move |r| match r {
                            Response::Pr { status } => Some(UiEvent::Pr { session, status }),
                            _ => None,
                        });
                        Ok(())
                    }
                    Cmd::Answer { session, request_id, decision } => client
                        .request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session, request_id, decision })
                        .await
                        .map(drop),
                    Cmd::Interrupt(session) => client.request(Request::SessionInterrupt { cmd_id: Uuid::new_v4(), session }).await.map(drop),
                    Cmd::MarkSeen { session, seq } => client.request(Request::MarkSeen { session, seq }).await.map(drop),
                    Cmd::Delete { session, remove_worktree } => {
                        if open.open == Some(session) {
                            open.open = None;
                        }
                        open.extra.retain(|s| *s != session);
                        client.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session, remove_worktree }).await.map(drop)
                    }
                };
                match result {
                    Ok(()) => {}
                    Err(ClientError::Rpc(e)) => {
                        if ui.send(UiEvent::Error(e.message)).is_err() {
                            return Ended::UiGone;
                        }
                    }
                    Err(_) => return Ended::Lost,
                }
            }
        }
    }
}

/// asked the daemon to hand over to the one on disk, once per run.
static ASKED_RESTART: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// set once the window shows pages natively: the daemon hears it on each connection.
static HOSTS_PAGES: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// a page's frame as pixels for the window: base64 JPEG to bgra.
fn decode_frame(jpeg: &str) -> Option<(Vec<u8>, u32, u32)> {
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD.decode(jpeg).ok()?;
    let img = image::load_from_memory_with_format(&bytes, image::ImageFormat::Jpeg).ok()?;
    let mut rgba = img.to_rgba8();
    for px in rgba.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let (w, h) = rgba.dimensions();
    Some((rgba.into_raw(), w, h))
}
