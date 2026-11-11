//! the connection to the daemon, on its own thread.
//!
//! the ui thread never waits on the socket. it drops commands into a channel
//! and receives `UiEvent`s from another. if the daemon goes away this keeps
//! reconnecting (starting one if needed) and re-subscribes to whatever was open.

use photon_client::{Client, ClientError, Events};
use photon_proto::{
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
    /// show this session (or none).
    Open(Option<SessionId>),
    Create { cwd: String, worktree: bool, prompt: String },
    /// start a session without opening it: it works beside what you are doing.
    CreateAside { cwd: String, goal: String, prompt: String, mode: String },
    Send { session: SessionId, text: String, attachments: Vec<Attachment> },
    SetModel { session: SessionId, model: String },
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
    Models(Vec<ModelInfo>),
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
    /// a session started aside exists, with this id.
    Aside(SessionId),
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
        .name("photon-bridge".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(run(home, cmd_rx, ui_tx));
        })
        .expect("bridge thread");
    (Bridge { cmds: cmd_tx }, ui_rx)
}

async fn run(home: PathBuf, mut cmds: mpsc::UnboundedReceiver<Cmd>, ui: mpsc::UnboundedSender<UiEvent>) {
    let mut open: Option<SessionId> = None;
    let mut backoff = Duration::from_millis(100);
    loop {
        match photon_client::connect_or_start(&home, "photon").await {
            Err(e) => {
                if ui.send(UiEvent::Down(e.to_string())).is_err() {
                    return;
                }
                // a wrong-version daemon won't fix itself quickly; don't spin on it.
                let wait = if e.is_version_mismatch() { Duration::from_secs(5) } else { backoff };
                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(Duration::from_secs(3));
            }
            Ok((client, events, _welcome)) => {
                let client = Arc::new(client);
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

async fn subscribe(client: &Arc<Client>, ui: &mpsc::UnboundedSender<UiEvent>, open: &mut Option<SessionId>) -> Result<(), ClientError> {
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
    open: &mut Option<SessionId>,
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
                        continue;
                    }
                    Event::UiCall { call, cmd, args } => UiEvent::Call { call, cmd, args },
                    Event::UiWatched { watched } => UiEvent::Watched(watched),
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
                        *open = id;
                        subscribe(client, ui, open).await
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
                                *open = Some(id);
                                subscribe(client, ui, open).await
                            }
                            Ok(_) => Ok(()),
                            Err(e) => Err(e),
                        }
                    }
                    Cmd::CreateAside { cwd, goal, prompt, mode } => {
                        let req = Request::SessionCreate {
                            cmd_id: Uuid::new_v4(),
                            cwd,
                            worktree: None,
                            goal: Some(goal),
                            prompt: None,
                            model: None,
                            parent: None,
                        };
                        match client.request(req).await {
                            Ok(Response::Session { summary }) => {
                                let id = summary.id;
                                let _ = ui.send(UiEvent::Session(summary));
                                let _ = ui.send(UiEvent::Aside(id));
                                // allowed to edit before it is asked anything, so it never stops to ask.
                                let _ = client.request(Request::SessionSetMode { cmd_id: Uuid::new_v4(), session: id, mode }).await;
                                client.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: id, text: prompt, attachments: vec![] }).await.map(drop)
                            }
                            Ok(_) => Ok(()),
                            Err(e) => Err(e),
                        }
                    }
                    Cmd::Send { session, text, attachments } => client.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session, text, attachments }).await.map(drop),
                    Cmd::SetModel { session, model } => client.request(Request::SessionSetModel { cmd_id: Uuid::new_v4(), session, model }).await.map(drop),
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
                            Response::Models { models } => Some(UiEvent::Models(models)),
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
                        ask(client, ui, Request::GitDiff { session, path }, move |r| match r {
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
                        if *open == Some(session) {
                            *open = None;
                        }
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
