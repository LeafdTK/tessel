//! the daemon end to end: a real socket, real clients, real storage, and a
//! stand-in agent replaying recorded claude sessions.

use tessel_agent::recording::{self, RecLine};
use tessel_agent::replay::ReplayBackend;
use tessel_client::{Client, Events};
use tessel_proto::*;
use tesseld::hub::{Config, HubMsg};
use tesseld::paths::Paths;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use uuid::Uuid;

fn fixture(name: &str) -> Vec<RecLine> {
    recording::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../tessel-agent/tests/fixtures").join(format!("{name}.jsonl"))).unwrap()
}

struct Daemon {
    cfg: Config,
    backend: Arc<ReplayBackend>,
    running: Option<tesseld::Running>,
    _tmp: Option<tempfile::TempDir>,
}

impl Daemon {
    async fn start(fixtures: &[&str]) -> Self {
        Self::start_with(fixtures, |_| {}).await
    }

    async fn start_with(fixtures: &[&str], tweak: impl FnOnce(&mut Config)) -> Self {
        // short path: unix socket paths are capped at ~104 bytes.
        let tmp = tempfile::Builder::new().prefix("ph").tempdir_in("/tmp").unwrap();
        let mut cfg = Config::new(Paths::at(tmp.path()));
        cfg.tick = Duration::from_millis(20);
        tweak(&mut cfg);
        let mut d = Self { cfg, backend: Arc::new(ReplayBackend::new(vec![fixture("plain")])), running: None, _tmp: Some(tmp) };
        d.boot(fixtures).await;
        d
    }

    /// (re)start the daemon on the same home with a fresh stand-in agent.
    async fn boot(&mut self, fixtures: &[&str]) {
        let again = fixtures.iter().any(|f| f.ends_with('+'));
        let replay = ReplayBackend::new(fixtures.iter().map(|f| fixture(f.trim_end_matches('+'))).collect());
        self.backend = Arc::new(if again { replay.again() } else { replay });
        // a daemon that has just stopped can still hold its lock for a moment;
        // a restart waits for it, as a client starting one does.
        let mut tries = 0;
        loop {
            match tesseld::start(self.cfg.clone(), self.backend.clone()).await {
                Ok(r) => break self.running = Some(r),
                Err(e) if tries < 100 && e.to_string().contains("already running") => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(e) => panic!("{e}"),
            }
        }
    }

    async fn stop(&mut self) {
        let running = self.running.take().unwrap();
        running.hub.send(HubMsg::Stop).ok();
        tokio::time::timeout(Duration::from_secs(5), running.done).await.expect("daemon did not stop").unwrap();
    }

    async fn client(&self) -> (Client, Events) {
        let (c, ev, _) = Client::connect(&self.cfg.paths.socket(), "test").await.unwrap();
        (c, ev)
    }

    fn cwd(&self) -> String {
        self.cfg.paths.home.to_string_lossy().into_owned()
    }

    fn written(&self) -> Vec<serde_json::Value> {
        self.backend.written.lock().unwrap().iter().map(|l| serde_json::from_str(l).unwrap()).collect()
    }
}

async fn create(c: &Client, cwd: &str, prompt: Option<&str>) -> SessionSummary {
    let req = Request::SessionCreate {
        cmd_id: Uuid::new_v4(), cwd: cwd.into(), worktree: None, goal: None, prompt: prompt.map(str::to_owned), model: None, parent: None, attachments: vec![]
    };
    match c.request(req).await.unwrap() {
        Response::Session { summary } => summary,
        other => panic!("{other:?}"),
    }
}

async fn subscribe(c: &Client, summary: bool, detail: Option<SessionId>) -> (Option<Vec<SessionSummary>>, Option<SessionSnapshot>) {
    match c.request(Request::Subscribe { summary, detail, tail: 1000 }).await.unwrap() {
        Response::Subscribed { sessions, detail } => (sessions, detail),
        other => panic!("{other:?}"),
    }
}

async fn list(c: &Client) -> Vec<SessionSummary> {
    match c.request(Request::SessionList).await.unwrap() {
        Response::Sessions { sessions } => sessions,
        other => panic!("{other:?}"),
    }
}

/// poll the daemon until a session's summary satisfies `pred`.
async fn until(c: &Client, id: SessionId, what: &str, pred: impl Fn(&SessionSummary) -> bool) -> SessionSummary {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let all = list(c).await;
        let s = all.iter().find(|s| s.id == id);
        if let Some(s) = s
            && pred(s)
        {
            return s.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}. last: {s:?}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

/// read events until one satisfies `pred`; everything read is returned.
/// the next envelope on a raw client queue (terminal bytes skipped).
async fn recv_env(rx: &mut mpsc::Receiver<tesseld::server::Out>) -> Option<Envelope> {
    loop {
        match rx.recv().await? {
            tesseld::server::Out::Env(e) => return Some(e),
            tesseld::server::Out::Term(..) => continue,
        }
    }
}

async fn events_until(ev: &mut Events, what: &str, mut pred: impl FnMut(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), ev.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for: {what}. saw {} events", seen.len()))
            .expect("connection closed");
        let hit = pred(&e);
        seen.push(e);
        if hit {
            return seen;
        }
    }
}

fn wants(s: &SessionSummary) -> Option<AttentionReason> {
    s.attention.as_ref().map(|a| a.reason)
}

async fn snapshot(c: &Client, id: SessionId) -> SessionSnapshot {
    subscribe(c, false, Some(id)).await.1.unwrap()
}

fn tool_statuses(snap: &SessionSnapshot) -> Vec<(String, ToolStatus)> {
    snap.items.iter().filter_map(|i| match &i.body { ItemBody::Tool { tool, status, .. } => Some((tool.clone(), *status)), _ => None }).collect()
}

// ---- handshake ------------------------------------------------------------------

#[tokio::test]
async fn nothing_works_before_hello_and_a_wrong_version_is_refused_with_a_reason() {
    use tessel_proto::frame::{Decoder, KIND_JSON, encode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let d = Daemon::start(&["plain"]).await;
    let mut stream = tokio::net::UnixStream::connect(d.cfg.paths.socket()).await.unwrap();
    let mut decoder = Decoder::new();
    let ask = async |stream: &mut tokio::net::UnixStream, decoder: &mut Decoder, id: u64, req: Request| -> Envelope {
        let json = serde_json::to_vec(&Envelope::Req { id, req }).unwrap();
        stream.write_all(&encode(KIND_JSON, &json).unwrap()).await.unwrap();
        let mut buf = [0u8; 4096];
        loop {
            if let Some(f) = decoder.next_frame().unwrap() {
                return serde_json::from_slice(&f.payload).unwrap();
            }
            let n = stream.read(&mut buf).await.unwrap();
            assert!(n > 0, "daemon hung up");
            decoder.push(&buf[..n]);
        }
    };

    let r = ask(&mut stream, &mut decoder, 1, Request::SessionList).await;
    assert!(matches!(r, Envelope::Res { id: 1, err: Some(RpcError { code: ErrorCode::NotReady, .. }), .. }), "{r:?}");

    let hello = |major| Request::Hello { proto_major: major, proto_minor: 0, build: "x".into(), client: "raw".into(), auth: None };
    let r = ask(&mut stream, &mut decoder, 2, hello(PROTO_MAJOR + 1)).await;
    let Envelope::Res { err: Some(e), .. } = r else { panic!("{r:?}") };
    assert_eq!(e.code, ErrorCode::VersionMismatch);
    assert!(e.message.contains(&format!("{PROTO_MAJOR}.{PROTO_MINOR}")), "should say what the daemon speaks: {}", e.message);

    // still not ready after a refused hello.
    let r = ask(&mut stream, &mut decoder, 3, Request::SessionList).await;
    assert!(matches!(r, Envelope::Res { err: Some(RpcError { code: ErrorCode::NotReady, .. }), .. }));

    let r = ask(&mut stream, &mut decoder, 4, hello(PROTO_MAJOR)).await;
    assert!(matches!(r, Envelope::Res { ok: Some(Response::Welcome { .. }), .. }), "{r:?}");
    let r = ask(&mut stream, &mut decoder, 5, Request::SessionList).await;
    assert!(matches!(r, Envelope::Res { ok: Some(Response::Sessions { .. }), .. }));

    // a request from the future gets an error, not a hang or a disconnect.
    let raw = br#"{"t":"req","id":6,"req":{"method":"session_teleport","to":"mars"}}"#;
    stream.write_all(&encode(KIND_JSON, raw).unwrap()).await.unwrap();
    let mut buf = [0u8; 4096];
    let r: Envelope = loop {
        if let Some(f) = decoder.next_frame().unwrap() {
            break serde_json::from_slice(&f.payload).unwrap();
        }
        let n = stream.read(&mut buf).await.unwrap();
        decoder.push(&buf[..n]);
    };
    assert!(matches!(r, Envelope::Res { id: 6, err: Some(RpcError { code: ErrorCode::Invalid, .. }), .. }), "{r:?}");

    // a known request with something new inside it (a newer client's browser act) is answered too,
    // with why: an agent's tool once waited half an hour on one of these.
    let raw = br#"{"t":"req","id":7,"req":{"method":"browser","session":"00000000-0000-0000-0000-000000000001","act":{"act":"teleport"}}}"#;
    stream.write_all(&encode(KIND_JSON, raw).unwrap()).await.unwrap();
    let r: Envelope = loop {
        if let Some(f) = decoder.next_frame().unwrap() {
            break serde_json::from_slice(&f.payload).unwrap();
        }
        let n = stream.read(&mut buf).await.unwrap();
        decoder.push(&buf[..n]);
    };
    let Envelope::Res { id: 7, err: Some(e), .. } = r else { panic!("{r:?}") };
    assert!(e.message.contains("older than the client"), "{}", e.message);
}

#[tokio::test]
async fn only_one_daemon_per_home() {
    let d = Daemon::start(&["plain"]).await;
    let err = tesseld::start(d.cfg.clone(), d.backend.clone()).await.err().expect("a second daemon must be refused");
    assert!(err.to_string().contains("already running"), "{err}");
    // and the first one is unharmed.
    let (c, _) = d.client().await;
    assert!(list(&c).await.is_empty());
}

// ---- the main flow ----------------------------------------------------------------

#[tokio::test]
async fn a_session_runs_blocks_on_a_prompt_and_finishes_once_answered() {
    let d = Daemon::start(&["perm_allow"]).await;
    let (c, mut ev) = d.client().await;
    let (sessions, _) = subscribe(&c, true, None).await;
    assert_eq!(sessions.unwrap().len(), 0);

    let s = create(&c, &d.cwd(), Some("please write the file")).await;
    assert_eq!(s.goal, "please write the file", "the goal defaults to the first prompt");
    assert_eq!(s.worktree, None);

    // it blocks on the prompt, says so, and says what it wants.
    let blocked = until(&c, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;
    assert_eq!(wants(&blocked), Some(AttentionReason::NeedsPermission));
    assert_eq!(blocked.activity.as_deref(), Some("wants to: write tessel-fixture text into out.txt"));
    assert!(blocked.warm);

    // the summary stream told us too, without asking.
    events_until(&mut ev, "summary event for the prompt", |e| {
        matches!(e, Event::SessionUpdated { summary } if summary.run_state == RunState::NeedsPermission)
    })
    .await;

    let snap = snapshot(&c, s.id).await;
    assert_eq!(snap.pending.len(), 1);
    assert_eq!(tool_statuses(&snap), [("Bash".into(), ToolStatus::AwaitingPermission)]);
    assert!(matches!(&snap.items[0].body, ItemBody::User { text, .. } if text == "please write the file"));

    let request_id = snap.pending[0].request_id.clone();
    let answer = Request::PermissionRespond {
        cmd_id: Uuid::new_v4(), session: s.id, request_id: request_id.clone(), decision: PermissionDecision::Allow { updated_input: None },
    };
    assert_eq!(c.request(answer.clone()).await.unwrap(), Response::Ack);
    // answering again (a resend) is harmless.
    assert_eq!(c.request(answer).await.unwrap(), Response::Ack);

    let done = until(&c, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(done.run_state, RunState::Idle);
    assert!(done.cost_usd > 0.0);
    assert_eq!(done.model.as_deref(), Some("claude-haiku-5-5"));

    let snap = snapshot(&c, s.id).await;
    assert!(snap.pending.is_empty());
    assert_eq!(tool_statuses(&snap), [("Bash".into(), ToolStatus::Ok)]);
    // the reply, then the turn it ended.
    assert!(matches!(&snap.items.last().unwrap().body, ItemBody::Turn { .. }), "a finished turn closes the transcript");
    let reply = snap.items.iter().rev().find(|i| !matches!(i.body, ItemBody::Turn { .. })).unwrap();
    assert!(matches!(&reply.body, ItemBody::Text { text, streaming: false } if text == "done"));
    assert_eq!(snap.items.iter().filter(|i| matches!(i.body, ItemBody::User { .. })).count(), 1, "our message appears once, not once more when echoed");

    // exactly what reached the agent: our message, then one answer.
    let written = d.written();
    assert_eq!(written.len(), 2, "the duplicate answer never reached the agent: {written:?}");
    assert_eq!(written[0]["message"]["content"], "please write the file");
    assert_eq!(written[1]["response"]["request_id"], request_id.0);
    assert_eq!(written[1]["response"]["response"]["behavior"], "allow");

    // every line the agent wrote is in the raw log.
    let raw = std::fs::read_to_string(d.cfg.paths.raw_log(&s.id)).unwrap();
    assert_eq!(raw.lines().count(), recording::out_lines(&fixture("perm_allow")).count());
}

#[tokio::test]
async fn streaming_reaches_a_watching_client_live_and_an_interrupt_stops_it_cleanly() {
    // the recording: an essay streams, the user interrupts part way through.
    let d = Daemon::start(&["interrupt_text"]).await;
    let (c, mut ev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    subscribe(&c, false, Some(s.id)).await;
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "write an essay".into(), attachments: vec![] }).await.unwrap();

    // text arrives while the turn is still running, not only at the end.
    let mut seen = events_until(&mut ev, "text to start streaming", |e| matches!(e, Event::ItemDelta { .. })).await;
    let mid = until(&c, s.id, "writing", |s| s.run_state == RunState::Working).await;
    assert_eq!(mid.activity.as_deref(), Some("writing"));

    c.request(Request::SessionInterrupt { cmd_id: Uuid::new_v4(), session: s.id }).await.unwrap();
    seen.extend(events_until(&mut ev, "the turn to end", |e| matches!(e, Event::SessionUpdated { summary } if summary.run_state == RunState::Idle)).await);

    let mut streamed = std::collections::HashMap::<ItemId, String>::new();
    for e in &seen {
        if let Event::ItemDelta { item_id, offset, text, .. } = e {
            let buf = streamed.entry(item_id.clone()).or_default();
            assert_eq!(*offset as usize, buf.len(), "a delta arrived out of place");
            buf.push_str(text);
        }
    }
    let (id, text) = streamed.iter().max_by_key(|(_, t)| t.len()).unwrap();
    assert!(text.len() > 200, "the essay streamed: {} bytes", text.len());

    let snap = snapshot(&c, s.id).await;
    let ItemBody::Text { text: stored, streaming } = &snap.items.iter().find(|i| &i.id == id).unwrap().body else { panic!() };
    assert!(!streaming, "nothing is left marked as still writing");
    assert_eq!(stored, text, "what streamed is what is stored");
    assert!(snap.items.iter().any(|i| matches!(&i.body, ItemBody::Notice { level: NoticeLevel::Info, text } if text == "interrupted")));
    assert_eq!(snap.summary.attention, None, "you interrupted it; it does not then nag you");
    assert!(snap.summary.warm, "an interrupt stops the turn, not the agent");
    let written = d.written();
    assert_eq!(written.last().unwrap()["request"]["subtype"], "interrupt");
}

#[tokio::test]
async fn two_clients_agree_and_either_can_answer() {
    let d = Daemon::start(&["perm_allow"]).await;
    let (a, mut a_ev) = d.client().await;
    let s = create(&a, &d.cwd(), Some("go")).await;
    subscribe(&a, true, Some(s.id)).await;
    until(&a, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;

    // a second client arrives late and sees the same thing.
    let (b, _b_ev) = d.client().await;
    let snap_b = snapshot(&b, s.id).await;
    let snap_a = snapshot(&a, s.id).await;
    assert_eq!(snap_a.items, snap_b.items);
    assert_eq!(snap_a.pending, snap_b.pending);
    assert_eq!(snap_b.pending.len(), 1);

    // b answers; a is told the prompt is gone without asking.
    let request_id = snap_b.pending[0].request_id.clone();
    b.request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session: s.id, request_id: request_id.clone(), decision: PermissionDecision::Allow { updated_input: None } })
        .await
        .unwrap();
    events_until(&mut a_ev, "a to hear the prompt was answered", |e| matches!(e, Event::PendingChanged { pending, .. } if pending.is_empty())).await;
    // a answering the same prompt late is a no-op, not an error.
    let late = a.request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session: s.id, request_id, decision: PermissionDecision::Deny { message: "too late".into() } }).await;
    assert_eq!(late.unwrap(), Response::Ack);

    let done = until(&a, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(tool_statuses(&snapshot(&a, s.id).await), [("Bash".into(), ToolStatus::Ok)], "the late deny changed nothing");
    assert_eq!(d.written().len(), 2);

    // b marks it seen; a's view clears too. the flag lives in the daemon, not in a window.
    b.request(Request::MarkSeen { session: s.id, seq: done.last_seq }).await.unwrap();
    events_until(&mut a_ev, "a to see the flag clear", |e| matches!(e, Event::SessionUpdated { summary } if summary.attention.is_none() && summary.run_state == RunState::Idle)).await;
}

#[tokio::test]
async fn a_resent_command_does_not_happen_twice() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _) = d.client().await;
    let cmd_id = Uuid::new_v4();
    let req = Request::SessionCreate { cmd_id, cwd: d.cwd(), worktree: None, goal: Some("once".into()), prompt: Some("hi".into()), model: None, parent: None, attachments: vec![] };
    let Response::Session { summary: first } = c.request(req.clone()).await.unwrap() else { panic!() };
    let Response::Session { summary: second } = c.request(req).await.unwrap() else { panic!() };
    assert_eq!(first.id, second.id);
    assert_eq!(list(&c).await.len(), 1);

    until(&c, first.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    let send = Request::SessionSend { cmd_id: Uuid::new_v4(), session: first.id, text: "again".into(), attachments: vec![] };
    c.request(send.clone()).await.unwrap();
    c.request(send).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let users = snapshot(&c, first.id).await.items.iter().filter(|i| matches!(i.body, ItemBody::User { .. })).count();
    assert_eq!(users, 2, "the first prompt and one follow-up, not two");
}

#[tokio::test]
async fn bad_requests_get_clear_errors() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _) = d.client().await;
    let ghost = SessionId::new();
    let code = |r: Result<Response, tessel_client::ClientError>| match r {
        Err(tessel_client::ClientError::Rpc(e)) => e.code,
        other => panic!("{other:?}"),
    };
    assert_eq!(code(c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: ghost, text: "x".into(), attachments: vec![] }).await), ErrorCode::NotFound);
    assert_eq!(code(c.request(Request::Subscribe { summary: false, detail: Some(ghost), tail: 1 }).await), ErrorCode::NotFound);
    assert_eq!(code(c.request(Request::MarkSeen { session: ghost, seq: 1 }).await), ErrorCode::NotFound);
    let no_dir = Request::SessionCreate { cmd_id: Uuid::new_v4(), cwd: "/definitely/not/here".into(), worktree: None, goal: None, prompt: None, model: None, parent: None, attachments: vec![] };
    assert_eq!(code(c.request(no_dir).await), ErrorCode::Invalid);
    let orphan = Request::SessionCreate { cmd_id: Uuid::new_v4(), cwd: d.cwd(), worktree: None, goal: None, prompt: None, model: None, parent: Some(ghost), attachments: vec![] };
    assert_eq!(code(c.request(orphan).await), ErrorCode::NotFound);
    let s = create(&c, &d.cwd(), None).await;
    assert_eq!(code(c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "   ".into(), attachments: vec![] }).await), ErrorCode::Invalid);
    // deleting twice is fine.
    let rm = || Request::SessionDelete { cmd_id: Uuid::new_v4(), session: s.id, remove_worktree: false };
    assert_eq!(c.request(rm()).await.unwrap(), Response::Ack);
    assert_eq!(c.request(rm()).await.unwrap(), Response::Ack);
    assert!(list(&c).await.is_empty());
}

// ---- surviving things ---------------------------------------------------------------

#[tokio::test]
async fn sessions_survive_the_client_going_away() {
    let d = Daemon::start(&["perm_allow"]).await;
    let id = {
        let (c, _ev) = d.client().await;
        let s = create(&c, &d.cwd(), Some("go")).await;
        until(&c, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;
        s.id
        // the "app" quits here, mid-turn, with a prompt pending.
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (c, _) = d.client().await;
    let s = until(&c, id, "still blocked", |s| s.run_state == RunState::NeedsPermission).await;
    assert!(s.warm, "the agent is still running with nobody watching");
    let snap = snapshot(&c, id).await;
    c.request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session: id, request_id: snap.pending[0].request_id.clone(), decision: PermissionDecision::Allow { updated_input: None } })
        .await
        .unwrap();
    until(&c, id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
}

#[tokio::test]
async fn a_daemon_restart_mid_turn_is_flagged_and_the_session_resumes() {
    let mut d = Daemon::start(&["perm_allow"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("go")).await;
    until(&c, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;
    let before = snapshot(&c, s.id).await;
    drop(c);

    d.stop().await;
    assert!(!d.cfg.paths.socket().exists(), "the socket is cleaned up on stop");
    d.boot(&["resume"]).await;

    let (c, _) = d.client().await;
    let back = until(&c, s.id, "the session to come back", |_| true).await;
    assert_eq!(back.run_state, RunState::Errored);
    assert_eq!(wants(&back), Some(AttentionReason::Errored), "a turn cut short by a restart must be flagged");
    assert!(!back.warm);
    assert_eq!(back.goal, "go");
    let snap = snapshot(&c, s.id).await;
    assert!(snap.pending.is_empty());
    assert_eq!(tool_statuses(&snap), [("Bash".into(), ToolStatus::Aborted)], "no spinner left behind");
    assert_eq!(snap.items[0], before.items[0], "history is intact");
    assert!(snap.items.iter().any(|i| matches!(i.body, ItemBody::Notice { level: NoticeLevel::Error, .. })));

    // carry on: the next message starts a new process that resumes the same conversation.
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "what were you doing?".into(), attachments: vec![] }).await.unwrap();
    let done = until(&c, s.id, "the resumed turn to finish", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(done.run_state, RunState::Idle);
    let spawned = d.backend.spawned.lock().unwrap().clone();
    assert_eq!(spawned.len(), 1);
    assert!(spawned[0].resume, "must resume, not start a new conversation");
    assert_eq!(spawned[0].agent_session_id, "622d2e2f-b0b1-4948-a775-1ee25604df99", "with the id the agent gave us before the restart");
    assert!(snapshot(&c, s.id).await.items.len() > snap.items.len());
}

#[tokio::test]
async fn an_agent_that_dies_mid_tool_is_closed_out_and_flagged() {
    // kill9: the recording ends with the process being killed while a tool runs.
    let d = Daemon::start_with(&["kill9"], |cfg| cfg.interrupt_grace = Duration::from_millis(60)).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("go")).await;
    until(&c, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;
    let snap = snapshot(&c, s.id).await;
    c.request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session: s.id, request_id: snap.pending[0].request_id.clone(), decision: PermissionDecision::Allow { updated_input: None } })
        .await
        .unwrap();
    let working = until(&c, s.id, "the tool to be running", |s| s.run_state == RunState::Working).await;
    assert_eq!(working.activity.as_deref(), Some("sleep for 40 seconds with python"));

    // interrupt. this agent ignores it, so after the grace period it is stopped outright.
    c.request(Request::SessionInterrupt { cmd_id: Uuid::new_v4(), session: s.id }).await.unwrap();
    let dead = until(&c, s.id, "the agent to be stopped", |s| !s.warm).await;
    assert_eq!(dead.run_state, RunState::Errored);
    assert_eq!(wants(&dead), Some(AttentionReason::Errored));
    assert_eq!(tool_statuses(&snapshot(&c, s.id).await), [("Bash".into(), ToolStatus::Aborted)]);
}

#[tokio::test]
async fn idle_agents_are_reaped_and_come_back_on_demand() {
    let d = Daemon::start_with(&["plain", "resume"], |cfg| cfg.idle_reap = Duration::from_millis(30)).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hi")).await;
    let cold = until(&c, s.id, "reaped", |s| !s.warm && wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(cold.run_state, RunState::Idle, "reaping an idle agent is not an error");
    assert!(!snapshot(&c, s.id).await.items.iter().any(|i| matches!(i.body, ItemBody::Notice { .. })), "and leaves no notice");

    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "still there?".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "second turn", |s| s.last_seq > cold.last_seq && wants(s) == Some(AttentionReason::Finished)).await;
    let spawned = d.backend.spawned.lock().unwrap().clone();
    assert_eq!(spawned.len(), 2);
    assert!(!spawned[0].resume);
    assert!(spawned[1].resume);
}

// ---- worktrees ----------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

#[tokio::test]
async fn a_session_can_be_given_its_own_worktree_and_is_otherwise_the_same_thing() {
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["commit", "-q", "--allow-empty", "-m", "init"]] {
        git(&repo, args);
    }
    let (c, _) = d.client().await;
    let req = |name: &str| Request::SessionCreate {
        cmd_id: Uuid::new_v4(), cwd: repo.to_string_lossy().into_owned(), worktree: Some(WorktreeSpec { base: None, name: Some(name.into()) }),
        goal: Some("isolated".into()), prompt: Some("hi".into()), model: None, parent: None, attachments: vec![],
    };
    let Response::Session { summary: s } = c.request(req("feature-x")).await.unwrap() else { panic!() };
    let wt = s.worktree.clone().expect("has a worktree");
    assert_eq!(wt.branch, "tessel/feature-x");
    assert_ne!(Path::new(&s.cwd), repo);
    assert!(Path::new(&s.cwd).starts_with(&d.cfg.paths.workspaces));
    assert_eq!(git(Path::new(&s.cwd), &["rev-parse", "--abbrev-ref", "HEAD"]), "tessel/feature-x");
    assert_eq!(git(&repo, &["rev-parse", "--abbrev-ref", "HEAD"]), "main", "the original checkout is untouched");
    // a new tab in that worktree is in that worktree: same repository, same branch.
    let tab = create(&c, &s.cwd, None).await;
    assert_eq!(tab.worktree.as_ref(), Some(&wt), "a tab is told whose worktree it's in");
    c.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session: tab.id, remove_worktree: false }).await.unwrap();

    // the agent runs in the worktree, and the session behaves like any other.
    until(&c, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(d.backend.spawned.lock().unwrap()[0].cwd, PathBuf::from(&s.cwd));

    // the same name again is a conflict, said plainly.
    match c.request(req("feature-x")).await {
        Err(tessel_client::ClientError::Rpc(e)) => assert_eq!(e.code, ErrorCode::Conflict),
        other => panic!("{other:?}"),
    }
    assert_eq!(list(&c).await.len(), 1, "a failed create leaves nothing behind");

    c.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session: s.id, remove_worktree: true }).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while Path::new(&s.cwd).exists() {
        assert!(tokio::time::Instant::now() < deadline, "worktree was not removed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(repo.exists());
}

async fn deleted(c: &Client) -> Vec<tessel_proto::DeletedSession> {
    let Response::Deleted { sessions } = c.request(Request::DeletedSessions).await.unwrap() else { panic!() };
    sessions
}

#[tokio::test]
async fn a_deleted_workspace_comes_back_with_its_work_until_it_is_deleted_for_good() {
    let mut d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["commit", "-q", "--allow-empty", "-m", "init"]] {
        git(&repo, args);
    }
    let (c, _) = d.client().await;
    let made = Request::SessionCreate {
        cmd_id: Uuid::new_v4(), cwd: repo.to_string_lossy().into_owned(), worktree: Some(WorktreeSpec { base: None, name: Some("keep-me".into()) }),
        goal: Some("half done".into()), prompt: None, model: None, parent: None, attachments: vec![],
    };
    let Response::Session { summary: s } = c.request(made).await.unwrap() else { panic!() };
    let ws = PathBuf::from(&s.cwd);
    std::fs::write(ws.join("notes.md"), "not committed yet\n").unwrap();

    c.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session: s.id, remove_worktree: true }).await.unwrap();
    assert!(list(&c).await.is_empty(), "gone from the list");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while ws.exists() || !deleted(&c).await.first().is_some_and(|x| x.kept_work) {
        assert!(tokio::time::Instant::now() < deadline, "the work wasn't kept and the worktree removed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let gone = deleted(&c).await;
    assert_eq!((gone.len(), gone[0].id, gone[0].goal.as_str(), gone[0].workspace_removed), (1, s.id, "half done", true));

    d.stop().await;
    d.boot(&["plain"]).await;
    let (c, _) = d.client().await;
    assert!(list(&c).await.is_empty(), "a restart doesn't bring it back by itself");
    assert_eq!(deleted(&c).await.len(), 1, "nor forget it");

    let Response::Session { summary: back } = c.request(Request::SessionRestore { session: s.id }).await.unwrap() else { panic!() };
    assert_eq!((back.id, back.cwd.as_str()), (s.id, s.cwd.as_str()));
    assert_eq!(std::fs::read_to_string(ws.join("notes.md")).unwrap(), "not committed yet\n", "with the work it had");
    assert_eq!(git(&ws, &["rev-parse", "--abbrev-ref", "HEAD"]), "tessel/keep-me");
    assert_eq!(list(&c).await.len(), 1);
    assert!(deleted(&c).await.is_empty());
    assert!(git(&repo, &["for-each-ref", "refs/tessel/archive"]).is_empty(), "what was kept is let go once it's back");

    c.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session: s.id, remove_worktree: false }).await.unwrap();
    c.request(Request::SessionPurge { session: s.id }).await.unwrap();
    assert!(deleted(&c).await.is_empty());
    d.stop().await;
    d.boot(&["plain"]).await;
    let (c, _) = d.client().await;
    assert!(list(&c).await.is_empty() && deleted(&c).await.is_empty(), "deleted for good");
}

// ---- slow clients ---------------------------------------------------------------------

#[tokio::test]
async fn a_client_that_cannot_keep_up_is_told_and_not_left_with_a_silently_stale_view() {
    let d = Daemon::start(&["interrupt_text"]).await;
    let hub = d.running.as_ref().unwrap().hub.clone();
    // a client with room for only a handful of messages, that we do not read from.
    let (tx, mut rx) = mpsc::channel::<tesseld::server::Out>(4);
    let me = 9_000_001;
    hub.send(HubMsg::Connected { client: me, tx }).unwrap();
    let hello = Request::Hello { proto_major: PROTO_MAJOR, proto_minor: PROTO_MINOR, build: "t".into(), client: "slow".into(), auth: None };
    hub.send(HubMsg::Request { client: me, id: 1, req: hello }).unwrap();
    assert!(matches!(recv_env(&mut rx).await, Some(Envelope::Res { id: 1, ok: Some(Response::Welcome { .. }), .. })));

    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    hub.send(HubMsg::Request { client: me, id: 2, req: Request::Subscribe { summary: true, detail: Some(s.id), tail: 10 } }).unwrap();
    assert!(matches!(recv_env(&mut rx).await, Some(Envelope::Res { id: 2, ok: Some(Response::Subscribed { .. }), .. })));

    // a long streamed reply: far more events than the slow client has room for.
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "essay".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "writing", |s| s.activity.as_deref() == Some("writing")).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.request(Request::SessionInterrupt { cmd_id: Uuid::new_v4(), session: s.id }).await.unwrap();
    until(&c, s.id, "the turn to end", |s| s.run_state == RunState::Idle && s.last_seq > 3).await;

    // drain what fit, then the daemon must say events were dropped.
    let mut got_lagged = false;
    let mut last = None;
    while let Ok(Some(env)) = tokio::time::timeout(Duration::from_millis(500), recv_env(&mut rx)).await {
        got_lagged |= matches!(env, Envelope::Ev { ev: Event::Lagged });
        last = Some(env);
    }
    assert!(got_lagged, "the client was never told it fell behind");
    assert!(matches!(last, Some(Envelope::Ev { ev: Event::Lagged })), "and nothing is sent after that until it resubscribes");

    // resubscribing gives a complete, current picture.
    hub.send(HubMsg::Request { client: me, id: 3, req: Request::Subscribe { summary: true, detail: Some(s.id), tail: 1000 } }).unwrap();
    let Some(Envelope::Res { id: 3, ok: Some(Response::Subscribed { detail: Some(snap), .. }), .. }) = recv_env(&mut rx).await else { panic!() };
    assert_eq!(snap.items, snapshot(&c, s.id).await.items);
    // the fast client was never affected.
    assert_eq!(list(&c).await.len(), 1);
}

// ---- the working directory, over the wire -----------------------------------------

#[tokio::test]
async fn a_client_can_see_what_changed_and_read_and_save_files_through_the_daemon() {
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(repo.join("src")).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("src/lib.rs"), "fn a() {}\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);

    let (c, _) = d.client().await;
    let s = create(&c, repo.to_str().unwrap(), None).await;

    // nothing changed yet.
    let Response::Git { summary } = c.request(Request::GitStatus { session: s.id }).await.unwrap() else { panic!() };
    assert!(summary.is_repo && summary.files.is_empty());
    assert_eq!(summary.branch.as_deref(), Some("main"));

    // list, read, save. every path is relative to the session's folder.
    let Response::Dir { entries } = c.request(Request::FsList { session: s.id, path: String::new() }).await.unwrap() else { panic!() };
    assert_eq!(entries.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["src"]);
    let Response::File { file } = c.request(Request::FsRead { session: s.id, path: "src/lib.rs".into() }).await.unwrap() else { panic!() };
    assert_eq!(file.text.as_deref(), Some("fn a() {}\n"));
    let save = Request::FsWrite { cmd_id: Uuid::new_v4(), session: s.id, path: "src/lib.rs".into(), content: "fn a() {}\nfn b() {}\n".into() };
    assert_eq!(c.request(save).await.unwrap(), Response::Ack);
    assert_eq!(std::fs::read_to_string(repo.join("src/lib.rs")).unwrap(), "fn a() {}\nfn b() {}\n");

    // and now it shows as a change, with a diff the viewer can draw.
    let Response::Git { summary } = c.request(Request::GitStatus { session: s.id }).await.unwrap() else { panic!() };
    assert_eq!(summary.files.len(), 1);
    assert_eq!((summary.files[0].path.as_str(), summary.files[0].status, summary.added, summary.removed), ("src/lib.rs", ChangeStatus::Modified, 1, 0));
    let Response::Diff { diff } = c.request(Request::GitDiff { session: s.id, path: "src/lib.rs".into(), at: None, index: None }).await.unwrap() else { panic!() };
    assert!(diff.unified.contains("+fn b() {}"));
    assert_eq!(diff.old.as_deref(), Some("fn a() {}\n"));

    // a path that leaves the session's folder is refused, whatever the request.
    let code = |r: Result<Response, tessel_client::ClientError>| match r {
        Err(tessel_client::ClientError::Rpc(e)) => e.code,
        other => panic!("{other:?}"),
    };
    assert_eq!(code(c.request(Request::FsRead { session: s.id, path: "../state.json".into() }).await), ErrorCode::Invalid);
    assert_eq!(code(c.request(Request::FsWrite { cmd_id: Uuid::new_v4(), session: s.id, path: "/tmp/tessel-escape".into(), content: "x".into() }).await), ErrorCode::Invalid);
    assert_eq!(code(c.request(Request::GitDiff { session: s.id, path: "../../etc/passwd".into(), at: None, index: None }).await), ErrorCode::Invalid);
    assert!(!std::path::Path::new("/tmp/tessel-escape").exists());
    // and a session that doesn't exist is said so.
    assert_eq!(code(c.request(Request::GitStatus { session: SessionId::new() }).await), ErrorCode::NotFound);

    // the pull request question always gets an answer a person can read.
    let Response::Pr { status } = c.request(Request::PrStatus { session: s.id }).await.unwrap() else { panic!() };
    assert!(status.pr.is_none() && status.note.is_some());
}

#[tokio::test]
async fn the_model_can_be_switched_mid_session_and_the_summary_follows_the_agent() {
    let d = Daemon::start(&["model"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), Some("which model are you?")).await;
    c.request(Request::SessionSetModel { cmd_id: Uuid::new_v4(), session: s.id, model: "sonnet".into() }).await.unwrap();
    let done = until(&c, s.id, "the turn to finish", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert_eq!(done.model.as_deref(), Some("claude-sonnet-5-5"), "the agent's name for it, not our alias");
    assert_eq!(done.context_window, Some(1_000_000));
    assert!(done.context_used.is_some_and(|n| n > 1_000 && n < 1_000_000), "{:?}", done.context_used);
    let asked = d.written().into_iter().find(|w| w["type"] == "control_request").expect("the agent was asked");
    assert_eq!(asked["request"], serde_json::json!({"subtype": "set_model", "model": "sonnet"}));
}

#[tokio::test]
async fn a_plan_is_offered_for_approval_and_work_starts_once_it_is_approved() {
    let d = Daemon::start(&["plan"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), Some("plan how to make hello.txt")).await;
    c.request(Request::SessionSetMode { cmd_id: Uuid::new_v4(), session: s.id, mode: "plan".into() }).await.unwrap();
    let blocked = until(&c, s.id, "a plan to review", |s| s.activity.as_deref() == Some("has a plan for you to review")).await;
    assert_eq!(blocked.permission_mode.as_deref(), Some("plan"));
    assert_eq!(wants(&blocked), Some(AttentionReason::NeedsPermission));
    let snap = snapshot(&c, s.id).await;
    let req = &snap.pending[0];
    assert!(req.plan.as_deref().is_some_and(|p| p.contains("hello.txt")) && !req.is_question);
    let approve = Request::PermissionRespond {
        cmd_id: Uuid::new_v4(), session: s.id, request_id: req.request_id.clone(), decision: PermissionDecision::Allow { updated_input: None },
    };
    c.request(approve).await.unwrap();
    // the recording goes on to ask leave to write the file.
    let next = until(&c, s.id, "the write to ask permission", |s| s.activity.as_deref().is_some_and(|a| a.starts_with("wants to:"))).await;
    assert_eq!(next.permission_mode.as_deref(), Some("default"), "approving the plan left plan mode");
}

#[tokio::test]
async fn a_model_and_mode_chosen_before_the_first_message_are_what_the_agent_starts_with() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    c.request(Request::SessionSetModel { cmd_id: Uuid::new_v4(), session: s.id, model: "opus".into() }).await.unwrap();
    c.request(Request::SessionSetMode { cmd_id: Uuid::new_v4(), session: s.id, mode: "plan".into() }).await.unwrap();
    let chosen = until(&c, s.id, "the choice to show", |s| s.permission_mode.as_deref() == Some("plan")).await;
    assert_eq!(chosen.model.as_deref(), Some("opus"));
    assert!(d.written().is_empty(), "nothing is running yet, so nothing was asked");
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "go".into(), attachments: vec![] }).await.unwrap();
    let opts = d.backend.spawned.lock().unwrap()[0].clone();
    assert_eq!((opts.model.as_deref(), opts.permission_mode.as_deref()), (Some("opus"), Some("plan")));
    let empty = Request::SessionSetModel { cmd_id: Uuid::new_v4(), session: s.id, model: " ".into() };
    assert!(matches!(c.request(empty).await, Err(tessel_client::ClientError::Rpc(e)) if e.code == ErrorCode::Invalid));
}

#[tokio::test]
async fn a_new_session_starts_on_tessels_default_model_not_the_agents() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    // the loadout's first, as it runs there.
    assert_eq!((s.model.as_deref(), s.effort.as_deref()), (Some("opus"), Some("high")));
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "go".into(), attachments: vec![] }).await.unwrap();
    let opts = d.backend.spawned.lock().unwrap()[0].clone();
    assert_eq!((opts.model.as_deref(), opts.effort.as_deref()), (Some("opus"), Some("high")), "the agent is told, so it never falls back to its own");
}

#[tokio::test]
async fn visual_mode_is_kept_with_the_session_and_reaches_the_agent() {
    let d = Daemon::start_with(&["plain"], |cfg| cfg.tools_bin = Some(PathBuf::from("/bin/echo"))).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    assert!(!s.visual);
    c.request(Request::SessionSetVisual { session: s.id, visual: true }).await.unwrap();
    until(&c, s.id, "visual mode on", |s| s.visual).await;
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "go".into(), attachments: vec![] }).await.unwrap();
    let opts = d.backend.spawned.lock().unwrap()[0].clone();
    assert!(opts.visual, "the agent starts knowing it's watched");
    // and every shell it starts goes through tessel-shell: tesselctl, by that name.
    let env = |k: &str| opts.env.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let prefix = PathBuf::from(env("CLAUDE_CODE_SHELL_PREFIX").expect("a shell prefix"));
    assert_eq!(prefix.file_name().unwrap(), "tessel-shell");
    assert_eq!(std::fs::read_link(&prefix).unwrap(), PathBuf::from("/bin/echo"));
    assert_eq!(env("TESSEL_SESSION"), Some(s.id.to_string()), "so it knows whose commands they are");
}

#[tokio::test]
async fn an_agents_own_commands_show_in_one_terminal_that_only_they_write_to() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let (window, _wev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let mirror = |command: &str| {
        let c = &c;
        let command = command.to_owned();
        async move {
            match c.request(Request::TermMirror { session: s.id, command }).await.unwrap() {
                Response::Term { info } => info,
                other => panic!("{other:?}"),
            }
        }
    };
    let first = mirror("cargo test").await;
    assert!(first.agent && first.session == Some(s.id), "the agent's, for the pane to follow");
    c.send_term_input(first.id, b"running 3 tests\nok\n").unwrap();
    // keys typed at it go nowhere: from a window, or as a script.
    window.send_term_input(first.id, b"typed").unwrap();
    assert!(window.request(Request::TermInput { term: first.id, text: "typed".into() }).await.is_err());
    let second = mirror("git status\ngit diff").await;
    assert_eq!(second.id, first.id, "one terminal, each command under the last");
    c.send_term_input(first.id, b"clean").unwrap();
    let text = |r: Response| match r {
        Response::TermText { text } => text,
        other => panic!("{other:?}"),
    };
    let screen = text(c.request(Request::TermWait { term: first.id, text: Some("clean".into()), timeout_ms: 5000 }).await.unwrap());
    assert!(screen.contains("$ cargo test\nrunning 3 tests\nok\n"), "{screen}");
    assert!(screen.contains("$ git status …") && !screen.contains("typed"), "{screen}");
    // closed, it goes; the next command gets a new one.
    c.request(Request::TermClose { term: first.id }).await.unwrap();
    assert_ne!(mirror("ls").await.id, first.id);
}

#[tokio::test]
async fn a_screenshot_goes_to_the_agent_with_the_message_and_other_files_are_refused_up_front() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let zip = Attachment { name: "logs.zip".into(), media_type: "application/zip".into(), data: "AAAA".into(), element: None };
    let refused = c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "see".into(), attachments: vec![zip] }).await;
    let err = refused.unwrap_err();
    assert!(err.to_string().contains("logs.zip"), "{err}");
    assert!(d.written().is_empty() && snapshot(&c, s.id).await.items.is_empty(), "a refused message leaves no trace");

    let shot = Attachment { name: "shot.png".into(), media_type: "image/png".into(), data: "AAAA".into(), element: None };
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "what is this?".into(), attachments: vec![shot] }).await.unwrap();
    let sent = &d.written()[0]["message"]["content"];
    assert_eq!(sent[0], serde_json::json!({"type": "text", "text": "what is this?"}));
    assert_eq!(sent[1], serde_json::json!({"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}));
    let snap = snapshot(&c, s.id).await;
    assert!(matches!(&snap.items[0].body, ItemBody::User { text, attachments } if text == "what is this?" && attachments == &["shot.png".to_owned()]));
}

#[tokio::test]
async fn a_new_session_can_start_with_a_picture() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let shot = Attachment { name: "shot.png".into(), media_type: "image/png".into(), data: "AAAA".into(), element: None };
    let req = Request::SessionCreate { cmd_id: Uuid::new_v4(), cwd: d.cwd(), worktree: None, goal: None, prompt: Some("fix this".into()), model: None, parent: None, attachments: vec![shot.clone()] };
    c.request(req).await.unwrap();
    let sent = &d.written()[0]["message"]["content"];
    assert_eq!(sent[1]["source"]["data"], "AAAA", "the picture goes with the first message");
    // what can't be attached is refused before anything is made.
    let zip = Attachment { name: "logs.zip".into(), media_type: "application/zip".into(), ..shot };
    let bad = Request::SessionCreate { cmd_id: Uuid::new_v4(), cwd: d.cwd(), worktree: None, goal: None, prompt: Some("x".into()), model: None, parent: None, attachments: vec![zip] };
    assert!(c.request(bad).await.is_err());
    let Response::Sessions { sessions } = c.request(Request::SessionList).await.unwrap() else { panic!() };
    assert_eq!(sessions.len(), 1);
}

#[tokio::test]
async fn a_linked_folder_reaches_the_agent_the_next_time_it_starts() {
    let d = Daemon::start_with(&["plain"], |c| c.idle_reap = Duration::from_secs(3600)).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hello")).await;
    until(&c, s.id, "the first turn to finish", |s| wants(s) == Some(AttentionReason::Finished)).await;
    assert!(d.backend.spawned.lock().unwrap()[0].add_dirs.is_empty());

    let other = tempfile::tempdir().unwrap();
    let other_path = other.path().to_string_lossy().into_owned();
    let missing = Request::SessionLinkDir { cmd_id: Uuid::new_v4(), session: s.id, path: "/no/such/folder".into() };
    assert!(matches!(c.request(missing).await, Err(tessel_client::ClientError::Rpc(e)) if e.code == ErrorCode::Invalid));
    let relative = Request::SessionLinkDir { cmd_id: Uuid::new_v4(), session: s.id, path: "src".into() };
    assert!(matches!(c.request(relative).await, Err(tessel_client::ClientError::Rpc(e)) if e.code == ErrorCode::Invalid));

    c.request(Request::SessionLinkDir { cmd_id: Uuid::new_v4(), session: s.id, path: other_path.clone() }).await.unwrap();
    let linked = until(&c, s.id, "the link to show", |s| !s.linked_dirs.is_empty()).await;
    assert_eq!(linked.linked_dirs, vec![other_path.clone()]);
    // a running agent can't learn a new folder: it is stopped while idle and comes back with it.
    until(&c, s.id, "the idle agent to be stopped", |s| !s.warm).await;
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "again".into(), attachments: vec![] }).await.unwrap();
    let second = d.backend.spawned.lock().unwrap()[1].clone();
    assert_eq!(second.add_dirs, vec![PathBuf::from(&other_path)]);
    assert!(second.resume, "and it is the same conversation");

    c.request(Request::SessionUnlinkDir { cmd_id: Uuid::new_v4(), session: s.id, path: other_path }).await.unwrap();
    until(&c, s.id, "the link to go", |s| s.linked_dirs.is_empty()).await;
}

#[tokio::test]
async fn the_models_on_offer_come_from_the_agent_not_a_list_we_made_up() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    match c.request(Request::ModelList).await.unwrap() {
        Response::Models { models, .. } => {
            assert!(models.iter().any(|m| m.value == "sonnet" && m.name == "Sonnet 5.5"), "{models:?}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn refreshing_tool_servers_asks_the_running_agent() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hello")).await;
    until(&c, s.id, "the turn to finish", |s| wants(s) == Some(AttentionReason::Finished)).await;
    c.request(Request::SessionRefreshMcp { session: s.id }).await.unwrap();
    assert!(d.written().iter().any(|w| w["request"]["subtype"] == "mcp_status"), "a refresh asks the running agent");
}

// ---- windows ---------------------------------------------------------------------

/// next event of interest, skipping anything else.
async fn next_ui(ev: &mut Events) -> Event {
    loop {
        let e = tokio::time::timeout(Duration::from_secs(5), ev.recv()).await.expect("no event in time").expect("connection closed");
        if matches!(e, Event::UiCall { .. } | Event::Ui { .. } | Event::UiWatched { .. }) {
            return e;
        }
    }
}

#[tokio::test]
async fn a_window_command_goes_through_the_daemon_and_its_answer_comes_back() {
    let mut d = Daemon::start(&["plain"]).await;
    let (cli, _) = d.client().await;

    // no window yet: said plainly, not hung.
    let err = cli.request(Request::UiExec { window: None, cmd: "pane.split".into(), args: serde_json::json!({}) }).await.unwrap_err();
    assert!(err.to_string().contains("no tessel window"), "{err}");

    let (win, mut win_ev) = d.client().await;
    win.request(Request::UiRegister { name: "main".into() }).await.unwrap();
    assert!(matches!(next_ui(&mut win_ev).await, Event::UiWatched { watched: false }), "nobody listens yet");

    // the window answers whatever it is asked, from its own task.
    let answering = tokio::spawn(async move {
        let Event::UiCall { call, cmd, args } = next_ui(&mut win_ev).await else { panic!("expected a call") };
        assert_eq!((cmd.as_str(), args["dir"].as_str()), ("pane.split", Some("right")));
        win.request(Request::UiAnswer { call, ok: Some(serde_json::json!({"pane": "pane-1"})), err: None }).await.unwrap();
        (win, win_ev)
    });
    let reply = cli.request(Request::UiExec { window: None, cmd: "pane.split".into(), args: serde_json::json!({"dir": "right"}) }).await.unwrap();
    assert_eq!(reply, Response::UiResult { value: serde_json::json!({"pane": "pane-1"}) });
    let (win, mut win_ev) = answering.await.unwrap();

    // a subscriber hears what the window publishes, and the window learns it is watched.
    let (watcher, mut watch_ev) = d.client().await;
    watcher.request(Request::UiSubscribe { kinds: vec!["layout_changed".into()] }).await.unwrap();
    assert!(matches!(next_ui(&mut win_ev).await, Event::UiWatched { watched: true }));
    win.request(Request::UiPublish { kind: "pane_focused".into(), data: serde_json::json!({}) }).await.unwrap();
    win.request(Request::UiPublish { kind: "layout_changed".into(), data: serde_json::json!({"panes": 2}) }).await.unwrap();
    match next_ui(&mut watch_ev).await {
        Event::Ui { window, kind, data } => assert_eq!((window.as_str(), kind.as_str(), data["panes"].as_i64()), ("main", "layout_changed", Some(2)), "only the kinds asked for"),
        other => panic!("{other:?}"),
    }

    // a window that errors: the caller gets the error.
    let answering = tokio::spawn(async move {
        let Event::UiCall { call, .. } = next_ui(&mut win_ev).await else { panic!() };
        win.request(Request::UiAnswer { call, ok: None, err: Some("no pane called nope".into()) }).await.unwrap();
        win
    });
    let err = cli.request(Request::UiExec { window: None, cmd: "pane.close".into(), args: serde_json::json!({"pane": "nope"}) }).await.unwrap_err();
    assert!(err.to_string().contains("no pane called nope"), "{err}");
    let win = answering.await.unwrap();

    // and a window that goes away mid-call doesn't leave the caller hanging.
    let pending = tokio::spawn(async move { cli.request(Request::UiExec { window: None, cmd: "ui.ls".into(), args: serde_json::Value::Null }).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(win);
    let err = tokio::time::timeout(Duration::from_secs(5), pending).await.expect("caller left hanging").unwrap().unwrap_err();
    assert!(err.to_string().contains("window closed"), "{err}");
    d.stop().await;
}

#[tokio::test]
async fn an_agents_browser_acts_go_to_the_window_that_shows_the_page() {
    let mut d = Daemon::start(&["plain"]).await;
    let (agent, _) = d.client().await;
    let s = create(&agent, "/tmp", None).await.id;
    let (win, mut win_ev) = d.client().await;
    win.request(Request::UiRegister { name: "main".into() }).await.unwrap();
    win.request(Request::BrowserHost { on: true }).await.unwrap();

    let answering = tokio::spawn(async move {
        let call = loop {
            let e = tokio::time::timeout(Duration::from_secs(5), win_ev.recv()).await.expect("no call in time").expect("closed");
            if let Event::BrowserCall { call, session, act, agent, first } = e {
                assert_eq!((session, act, agent), (s, tessel_proto::BrowserAct::Read, true));
                assert!(first.starts_with("http://localhost:"), "where a new page starts: {first}");
                break call;
            }
        };
        let state = serde_json::json!({"url": "http://localhost:41000/", "title": "Home"});
        win.request(Request::UiAnswer { call, ok: Some(serde_json::json!({"state": state, "text": "Home"})), err: None }).await.unwrap();
        (win, win_ev)
    });
    let reply = agent.request(Request::Browser { session: s, act: tessel_proto::BrowserAct::Read, agent: true }).await.unwrap();
    let Response::Browser { state, text, image } = reply else { panic!("{reply:?}") };
    assert_eq!((state.title.as_str(), text.as_deref(), image), ("Home", Some("Home"), None));
    let (win, mut win_ev) = answering.await.unwrap();

    // once the window stops showing pages, it isn't asked: the daemon's own engine is used again.
    win.request(Request::BrowserHost { on: false }).await.unwrap();
    let pending = tokio::spawn(async move { agent.request(Request::Browser { session: s, act: tessel_proto::BrowserAct::Read, agent: true }).await });
    let asked = tokio::time::timeout(Duration::from_millis(800), async {
        loop {
            match win_ev.recv().await {
                Some(Event::BrowserCall { .. }) => return true,
                Some(_) => continue,
                None => return false,
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(!asked, "the window was asked after it stopped showing pages");
    pending.abort();
    d.stop().await;
}

#[tokio::test]
async fn any_byte_stream_is_a_connection_so_a_remote_transport_only_has_to_carry_bytes() {
    let mut d = Daemon::start(&["plain"]).await;
    // an in-memory pipe stands in for a quic stream or a websocket.
    let (near, far) = tokio::io::duplex(64 * 1024);
    assert!(tesseld::server::serve_stream(far, &d.running.as_ref().unwrap().hub));
    let (client, _events, welcome) = Client::over(near, "remote").await.unwrap();
    assert_eq!(welcome.proto_major, PROTO_MAJOR);
    let s = create(&client, &d.cwd(), None).await;
    match client.request(Request::SessionList).await.unwrap() {
        Response::Sessions { sessions } => assert!(sessions.iter().any(|x| x.id == s.id)),
        other => panic!("{other:?}"),
    }
    d.stop().await;
}

#[tokio::test]
async fn an_agent_draws_a_surface_changes_it_in_place_and_hears_what_was_clicked() {
    use tessel_proto::surface::SurfaceOp;
    let mut d = Daemon::start(&["plain", "plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let node = serde_json::json!({"k": "column", "c": [{"k": "progress", "id": "bar", "p": {"value": 0.2}}, {"k": "button", "id": "go", "p": {"label": "Go"}}]});
    c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Put { surface: "build".into(), node, region: "inline".into() } }).await.unwrap();
    c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Set { surface: "build".into(), element: "bar".into(), props: serde_json::json!({"value": 0.8}) } }).await.unwrap();

    let (_, snap) = subscribe(&c, false, Some(s.id)).await;
    let items = snap.unwrap().items;
    let drawn: Vec<_> = items.iter().filter(|i| matches!(i.body, ItemBody::Surface { .. })).collect();
    assert_eq!(drawn.len(), 1, "changed in place, not drawn twice");
    let ItemBody::Surface { node, .. } = &drawn[0].body else { unreachable!() };
    assert_eq!(node["c"][0]["p"]["value"], 0.8);

    // mistakes are refused with what's wrong.
    let err = c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Put { surface: "x".into(), node: serde_json::json!({"k": "buton"}), region: "inline".into() } }).await.unwrap_err();
    assert!(err.to_string().contains("isn't an element"), "{err}");
    let err = c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Set { surface: "build".into(), element: "nope".into(), props: serde_json::json!({}) } }).await.unwrap_err();
    assert!(err.to_string().contains("no element \"nope\""), "{err}");
    let err = c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Remove { surface: "never".into() } }).await.unwrap_err();
    assert!(err.to_string().contains("no surface"), "{err}");

    // a click reaches the agent as a message in its own session.
    c.request(Request::SurfaceEvent { session: s.id, surface: "build".into(), element: "go".into(), act: "click".into(), value: serde_json::Value::Null }).await.unwrap();
    let wrote = d.written();
    assert!(wrote.iter().any(|w| w.to_string().contains("surface-event") && w.to_string().contains("element=\\\"go\\\"")), "{wrote:?}");

    c.request(Request::SurfaceOp { session: s.id, op: SurfaceOp::Remove { surface: "build".into() } }).await.unwrap();
    let (_, snap) = subscribe(&c, false, Some(s.id)).await;
    assert!(snap.unwrap().items.iter().any(|i| matches!(&i.body, ItemBody::Surface { node, .. } if node.is_null())), "removed, and the record says so");
    d.stop().await;
}

#[tokio::test]
async fn turning_agent_tools_off_is_kept_told_to_everyone_and_reaches_the_next_agent() {
    let mut d = Daemon::start_with(&["plain", "plain"], |cfg| cfg.tools_bin = Some(PathBuf::from("/bin/echo"))).await;
    let (c, _) = d.client().await;
    let (other, mut other_ev) = d.client().await;
    let settings = |r: Response| match r {
        Response::Settings { settings } => settings,
        other => panic!("{other:?}"),
    };
    assert!(settings(c.request(Request::Settings).await.unwrap()).agent_tools, "on unless turned off");

    let spawned = |d: &Daemon| d.backend.spawned.lock().unwrap().iter().map(|o| o.tools.is_some()).collect::<Vec<_>>();
    let wait_spawns = |d: &Daemon, n: usize| {
        let backend = d.backend.clone();
        async move {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            while backend.spawned.lock().unwrap().len() < n {
                assert!(tokio::time::Instant::now() < deadline, "no agent started");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        }
    };
    create(&c, &d.cwd(), Some("hi")).await;
    wait_spawns(&d, 1).await;

    let now = settings(c.request(Request::SetSettings { patch: serde_json::json!({"agent_tools": false}) }).await.unwrap());
    assert!(!now.agent_tools);
    events_until(&mut other_ev, "the change, told to another client", |e| matches!(e, Event::Settings { settings } if !settings.agent_tools)).await;
    drop(other);

    // mistakes are refused, and change nothing.
    let err = c.request(Request::SetSettings { patch: serde_json::json!({"agent_toolz": true}) }).await.unwrap_err();
    assert!(err.to_string().contains("there's no setting \"agent_toolz\""), "{err}");
    let err = c.request(Request::SetSettings { patch: serde_json::json!({"agent_tools": "no"}) }).await.unwrap_err();
    assert!(err.to_string().contains("isn't a setting's value"), "{err}");

    create(&c, &d.cwd(), Some("hi again")).await;
    wait_spawns(&d, 2).await;
    assert_eq!(spawned(&d), [true, false], "the agent started after gets no tools");

    // kept across a restart.
    drop(c);
    d.stop().await;
    d.boot(&["plain"]).await;
    let (c, _) = d.client().await;
    assert!(!settings(c.request(Request::Settings).await.unwrap()).agent_tools);
    d.stop().await;
}

#[tokio::test]
async fn a_terminal_runs_in_the_daemon_and_any_client_can_watch_type_read_and_wait_on_it() {
    let mut d = Daemon::start(&["plain"]).await;
    let (c, mut ev) = d.client().await;
    let script = "printf 'ready\\n'; read a; printf 'got:%s\\n' \"$a\"; read b; printf 'then:%s\\n' \"$b\"; exit 3";
    let info = match c.request(Request::TermOpen { session: None, cwd: Some(d.cwd()), command: vec![script.into()], cols: 40, rows: 10, agent: false }).await.unwrap() {
        Response::Term { info } => info,
        other => panic!("{other:?}"),
    };
    events_until(&mut ev, "everyone hears it opened", |e| matches!(e, Event::TermUpdated { info: i } if i.id == info.id)).await;

    let text = |r: Response| match r {
        Response::TermText { text } => text,
        other => panic!("{other:?}"),
    };
    let ready = text(c.request(Request::TermWait { term: info.id, text: Some("ready".into()), timeout_ms: 5000 }).await.unwrap());
    assert!(ready.contains("ready"), "{ready}");

    // attaching starts with everything already on it, as bytes.
    c.request(Request::TermAttach { term: info.id }).await.unwrap();
    let mut seen = Vec::new();
    events_until(&mut ev, "the dump", |e| {
        if let Event::TermOutput { term, bytes } = e
            && *term == info.id
        {
            seen.extend_from_slice(bytes);
        }
        String::from_utf8_lossy(&seen).contains("ready")
    })
    .await;

    // what's running in it: the program in front, and where.
    match c.request(Request::TermProcess { term: info.id }).await.unwrap() {
        Response::TermProcess { pid: Some(pid), cwd, .. } => assert!(pid > 0 && cwd.is_some(), "{pid} {cwd:?}"),
        other => panic!("{other:?}"),
    }

    // typed as a window types (a binary frame), and as a script does.
    c.send_term_input(info.id, b"one\r").unwrap();
    text(c.request(Request::TermWait { term: info.id, text: Some("got:one".into()), timeout_ms: 5000 }).await.unwrap());
    c.request(Request::TermInput { term: info.id, text: "two\r".into() }).await.unwrap();
    let exited = match c.request(Request::TermWait { term: info.id, text: None, timeout_ms: 5000 }).await.unwrap() {
        Response::Term { info } => info,
        other => panic!("{other:?}"),
    };
    assert_eq!(exited.exited, Some(3), "its exit status");
    let screen = text(c.request(Request::TermCapture { term: info.id, history: true, format: None }).await.unwrap());
    assert!(screen.contains("got:one") && screen.contains("then:two"), "{screen}");
    // the same, as HTML, and nothing running in it now.
    let html = text(c.request(Request::TermCapture { term: info.id, history: true, format: Some("html".into()) }).await.unwrap());
    assert!(html.contains("<div") && html.contains("got:one"), "{html}");
    assert!(c.request(Request::TermCapture { term: info.id, history: true, format: Some("pdf".into()) }).await.is_err());
    assert!(matches!(c.request(Request::TermProcess { term: info.id }).await.unwrap(), Response::TermProcess { pid: None, .. }));

    // an attached client was streamed the rest.
    events_until(&mut ev, "the output, live", |e| {
        if let Event::TermOutput { bytes, .. } = e {
            seen.extend_from_slice(bytes);
        }
        String::from_utf8_lossy(&seen).contains("then:two")
    })
    .await;

    // waiting for what never comes ends with why: here, at once, as its program has ended.
    let err = c.request(Request::TermWait { term: info.id, text: Some("never".into()), timeout_ms: 5000 }).await.unwrap_err();
    assert!(err.to_string().contains("without showing \"never\""), "{err}");

    // an exited terminal stays until closed, so its output can still be read.
    let listed = match c.request(Request::TermList).await.unwrap() {
        Response::Terms { terms } => terms,
        other => panic!("{other:?}"),
    };
    assert_eq!(listed.len(), 1);
    // told not to stay, an exited terminal goes at once.
    c.request(Request::TermKeepOpen { term: info.id, on: false }).await.unwrap();
    events_until(&mut ev, "and then it's gone", |e| matches!(e, Event::TermRemoved { term } if *term == info.id)).await;
    assert!(c.request(Request::TermCapture { term: info.id, history: false, format: None }).await.is_err());
    d.stop().await;
}

#[tokio::test]
async fn closing_a_running_terminal_hangs_it_up() {
    let mut d = Daemon::start(&["plain"]).await;
    let (c, mut ev) = d.client().await;
    let info = match c.request(Request::TermOpen { session: None, cwd: Some(d.cwd()), command: vec!["sleep 600".into()], cols: 40, rows: 10, agent: false }).await.unwrap() {
        Response::Term { info } => info,
        other => panic!("{other:?}"),
    };
    c.request(Request::TermClose { term: info.id }).await.unwrap();
    events_until(&mut ev, "hung up and forgotten", |e| matches!(e, Event::TermRemoved { term } if *term == info.id)).await;
    d.stop().await;
}

#[tokio::test]
async fn history_staging_by_lines_commits_and_undo_all_go_through_the_daemon() {
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("graph");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("a.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    let (c, _) = d.client().await;
    let s = create(&c, repo.to_str().unwrap(), None).await;
    let tree = |r: Response| match r {
        Response::WorkTree { tree } => tree,
        other => panic!("{other:?}"),
    };
    let index_of = |path: &str| String::from_utf8(std::process::Command::new("git").arg("-C").arg(&repo).args(["show", &format!(":{path}")]).output().unwrap().stdout).unwrap();

    // two separate changes in one file; stage only the second, by its lines.
    std::fs::write(repo.join("a.txt"), "ONE\ntwo\nthree\nFOUR\nfive\n").unwrap();
    std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();
    let t = tree(c.request(Request::GitWorkTree { session: s.id }).await.unwrap());
    assert!(t.staged.is_empty());
    assert_eq!(t.unstaged.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["a.txt", "new.txt"]);
    let t = tree(c.request(Request::GitStageLines { session: s.id, path: "a.txt".into(), start: 4, end: 5, unstage: false, old: false }).await.unwrap());
    assert_eq!(index_of("a.txt"), "one\ntwo\nthree\nFOUR\nfive\n", "only the lines asked for");
    assert_eq!(t.undo.as_deref(), Some("Stage lines 4–5 of a.txt"));
    assert!(t.staged.iter().any(|f| f.path == "a.txt") && t.unstaged.iter().any(|f| f.path == "a.txt"), "partly staged: in both lists");
    // what's left to stage, and what is staged, each as its own diff.
    let side = |r: Response| match r {
        Response::Diff { diff } => diff.unified,
        other => panic!("{other:?}"),
    };
    let unstaged = side(c.request(Request::GitDiff { session: s.id, path: "a.txt".into(), at: None, index: Some(false) }).await.unwrap());
    let staged = side(c.request(Request::GitDiff { session: s.id, path: "a.txt".into(), at: None, index: Some(true) }).await.unwrap());
    assert!(unstaged.contains("+ONE") && !unstaged.contains("+FOUR"), "{unstaged}");
    assert!(staged.contains("+FOUR") && !staged.contains("+ONE"), "{staged}");

    // and a whole file, then take that back.
    tree(c.request(Request::GitStage { session: s.id, paths: vec!["new.txt".into()] }).await.unwrap());
    let t = tree(c.request(Request::GitUndo { session: s.id }).await.unwrap());
    assert!(!t.staged.iter().any(|f| f.path == "new.txt"), "undone");
    assert_eq!(t.undo.as_deref(), Some("Stage lines 4–5 of a.txt"), "the one before is next");

    // a message for what's staged, from a small model, over the staged diff.
    let Response::Suggested { message } = c.request(Request::GitSuggestMessage { session: s.id }).await.unwrap() else { panic!() };
    assert!(!message.is_empty());
    let asked = d.backend.asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, "haiku");
    assert!(asked[0].0.contains("+FOUR") && !asked[0].0.contains("+ONE"), "the staged diff, not the rest");

    let Response::Committed { sha } = c.request(Request::GitCommit { session: s.id, message: "second".into() }).await.unwrap() else { panic!() };
    let Response::GitLog { log } = c.request(Request::GitLog { session: s.id, skip: 0, limit: 10 }).await.unwrap() else { panic!() };
    assert_eq!(log.commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["second", "first"]);
    assert_eq!(log.commits[0].sha, sha);
    assert_eq!(log.commits[0].parents, [log.commits[1].sha.clone()]);
    assert!(log.commits[0].refs.iter().any(|r| r == "HEAD -> main"), "{:?}", log.commits[0].refs);
    assert!(!log.more);
    let Response::Changed { files } = c.request(Request::GitShow { session: s.id, sha: sha.clone() }).await.unwrap() else { panic!() };
    assert_eq!(files.iter().map(|f| (f.path.as_str(), f.added, f.removed)).collect::<Vec<_>>(), [("a.txt", 2, 1)]);
    let Response::Diff { diff } = c.request(Request::GitDiff { session: s.id, path: "a.txt".into(), at: Some(sha.clone()), index: None }).await.unwrap() else { panic!() };
    assert_eq!(diff.old.as_deref(), Some("one\ntwo\nthree\nfour\n"));
    assert_eq!(diff.new.as_deref(), Some("one\ntwo\nthree\nFOUR\nfive\n"));

    // undoing the commit puts its changes back to staged; then the line staging.
    let t = tree(c.request(Request::GitUndo { session: s.id }).await.unwrap());
    assert!(t.staged.iter().any(|f| f.path == "a.txt"));
    let Response::GitLog { log } = c.request(Request::GitLog { session: s.id, skip: 0, limit: 10 }).await.unwrap() else { panic!() };
    assert_eq!(log.commits.len(), 1);
    tree(c.request(Request::GitUndo { session: s.id }).await.unwrap());
    assert_eq!(index_of("a.txt"), "one\ntwo\nthree\nfour\n");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "ONE\ntwo\nthree\nFOUR\nfive\n", "the folder was never touched");

    // nothing left: undo says so. something moved behind tessel's back: undo refuses.
    assert!(c.request(Request::GitUndo { session: s.id }).await.is_err());
    tree(c.request(Request::GitStage { session: s.id, paths: vec!["new.txt".into()] }).await.unwrap());
    std::fs::write(repo.join("new.txt"), "changed\n").unwrap();
    git(&repo, &["add", "new.txt"]);
    let err = c.request(Request::GitUndo { session: s.id }).await.unwrap_err();
    assert!(err.to_string().contains("staged differently since"), "{err}");
    assert!(c.request(Request::GitStage { session: s.id, paths: vec!["../escape".into()] }).await.is_err(), "paths stay inside");
}

#[tokio::test]
async fn a_file_that_isnt_text_gets_something_to_look_at() {
    use base64::Engine as _;
    let d = Daemon::start(&["plain"]).await;
    let dir = d.cfg.paths.home.join("media");
    std::fs::create_dir_all(&dir).unwrap();
    // the smallest valid png: one transparent pixel.
    let png = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==").unwrap();
    std::fs::write(dir.join("dot.png"), &png).unwrap();
    std::fs::write(dir.join("notes.txt"), "hello from a text file\n".repeat(20)).unwrap();
    let (c, _) = d.client().await;
    let s = create(&c, dir.to_str().unwrap(), None).await;

    let Response::Preview { mime, data, note } = c.request(Request::FsPreview { session: s.id, path: "dot.png".into() }).await.unwrap() else { panic!() };
    assert_eq!(mime, "image/png");
    assert_eq!(base64::engine::general_purpose::STANDARD.decode(data).unwrap(), png, "an image is sent as it is");
    assert!(note.is_none());

    // anything else: a thumbnail, made by the daemon.
    let Response::Preview { mime, data, note } = c.request(Request::FsPreview { session: s.id, path: "notes.txt".into() }).await.unwrap() else { panic!() };
    assert_eq!(mime, "image/png");
    assert!(base64::engine::general_purpose::STANDARD.decode(data).unwrap().starts_with(b"\x89PNG"));
    assert!(note.is_some());

    assert!(c.request(Request::FsPreview { session: s.id, path: "../../etc/hosts".into() }).await.is_err());

    // every file, for finding by name.
    std::fs::create_dir_all(dir.join("node_modules/x")).unwrap();
    std::fs::write(dir.join("node_modules/x/skip.js"), "").unwrap();
    let Response::Paths { paths, more } = c.request(Request::FsIndex { session: s.id }).await.unwrap() else { panic!() };
    assert_eq!(paths, ["dot.png", "notes.txt"], "the heavy folders are left out");
    assert!(!more);
}

#[tokio::test]
async fn removed_lines_are_staged_by_where_they_were() {
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("removed");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("f.txt"), "a\nb\nc\nd\ne\n").unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    // two lines gone: b and d.
    std::fs::write(repo.join("f.txt"), "a\nc\ne\n").unwrap();
    let (c, _) = d.client().await;
    let s = create(&c, repo.to_str().unwrap(), None).await;
    c.request(Request::GitStageLines { session: s.id, path: "f.txt".into(), start: 4, end: 4, unstage: false, old: true }).await.unwrap();
    let index = git(&repo, &["show", ":f.txt"]);
    assert_eq!(index, "a\nb\nc\ne", "only d's removal is staged");
}

#[tokio::test]
async fn effort_and_fast_mode_are_kept_reach_the_agent_and_survive_a_restart() {
    let mut d = Daemon::start(&["plain", "plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let set_effort = |e: Option<&str>| Request::SessionSetEffort { cmd_id: Uuid::new_v4(), session: s.id, effort: e.map(str::to_owned) };

    // before the agent starts: kept, and it starts with them.
    c.request(set_effort(Some("xhigh"))).await.unwrap();
    c.request(Request::SessionSetFast { cmd_id: Uuid::new_v4(), session: s.id, fast: true }).await.unwrap();
    assert!(c.request(set_effort(Some("extreme"))).await.unwrap_err().to_string().contains("effort is one of"));
    let summary = until(&c, s.id, "the summary says so", |s| s.effort.as_deref() == Some("xhigh") && s.fast).await;
    assert!(summary.fast);
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "hi".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "the turn to end", |s| s.run_state == RunState::Idle && s.last_seq > 1).await;
    let opts = d.backend.spawned.lock().unwrap().last().cloned().unwrap();
    assert_eq!((opts.effort.as_deref(), opts.fast), (Some("xhigh"), true));

    // with it running: told at once.
    c.request(set_effort(Some("low"))).await.unwrap();
    let told = d.written().iter().any(|w| w["request"]["subtype"] == "apply_flag_settings" && w["request"]["settings"]["effortLevel"] == "low");
    assert!(told, "{:?}", d.written());

    // the model list says what each takes, with the slash commands.
    let Response::Models { models, commands } = c.request(Request::ModelList).await.unwrap() else { panic!() };
    assert!(models.iter().any(|m| m.value == "opus" && m.fast && m.efforts.len() == 5));
    assert!(commands.iter().any(|c| c.name == "review"));

    // a loadout of six is refused.
    let six: Vec<serde_json::Value> = (0..6).map(|i| serde_json::json!({"model": format!("m{i}")})).collect();
    assert!(c.request(Request::SetSettings { patch: serde_json::json!({"models": six}) }).await.unwrap_err().to_string().contains("at most 5"));

    drop(c);
    d.stop().await;
    d.boot(&["plain"]).await;
    let (c, _) = d.client().await;
    let back = list(&c).await.into_iter().find(|x| x.id == s.id).unwrap();
    assert_eq!((back.effort.as_deref(), back.fast), (Some("low"), true));
    d.stop().await;
}

#[tokio::test]
async fn a_conversation_forks_into_a_new_tab_that_carries_on_from_it() {
    let d = Daemon::start(&["plain", "plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let fork = |id| Request::SessionFork { cmd_id: Uuid::new_v4(), session: id, workspace: false, before: None };
    // nothing said yet: nothing to fork.
    assert!(c.request(fork(s.id)).await.unwrap_err().to_string().contains("send it a message first"));

    c.request(Request::SessionSetEffort { cmd_id: Uuid::new_v4(), session: s.id, effort: Some("high".into()) }).await.unwrap();
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "hi".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "the turn to end", |s| s.run_state == RunState::Idle && s.last_seq > 1).await;
    let parent = snapshot(&c, s.id).await;
    // the conversation id the parent's agent reported (with claude, the one tessel gave it;
    // the recording keeps its own).
    let parent_agent = "316819a3-be7f-4600-99dc-fb6704568a83".to_owned();

    let Response::Session { summary: child } = c.request(fork(s.id)).await.unwrap() else { panic!() };
    assert_ne!(child.id, s.id);
    assert_eq!((child.cwd.as_str(), child.effort.as_deref()), (s.cwd.as_str(), Some("high")), "a tab: the same workspace, the same settings");
    assert!(child.goal.ends_with("(fork)"));
    let copied = snapshot(&c, child.id).await;
    assert_eq!(copied.items.len(), parent.items.len(), "it shows the history it carries on from");

    // its agent starts from the parent's conversation, under a new id.
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: child.id, text: "and then?".into(), attachments: vec![] }).await.unwrap();
    until(&c, child.id, "the fork's turn to end", |s| s.run_state == RunState::Idle && s.last_seq > copied.summary.last_seq).await;
    let opts = d.backend.spawned.lock().unwrap().last().cloned().unwrap();
    assert!(opts.resume && opts.fork, "resumed and forked");
    assert_eq!(opts.agent_session_id, parent_agent);
}

#[tokio::test]
async fn a_chat_started_empty_is_named_by_its_first_message() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    assert_eq!(s.goal, "untitled");
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "tidy the readme please".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "a name from what was said", |s| s.goal == "tidy the readme please").await;
}

#[tokio::test]
async fn one_window_follows_several_chats_and_hears_from_each() {
    let d = Daemon::start(&["plain"]).await;
    let (c, mut ev) = d.client().await;
    let a = create(&c, &d.cwd(), None).await;
    let b = create(&c, &d.cwd(), None).await;
    let ghost = SessionId::new();

    let follow = |sessions: Vec<SessionId>| c.request(Request::Follow { sessions, tail: 50 });
    match follow(vec![a.id, ghost]).await.unwrap() {
        Response::Followed { snapshots, missing } => {
            assert_eq!(snapshots.iter().map(|s| s.summary.id).collect::<Vec<_>>(), vec![a.id]);
            assert_eq!(missing, vec![ghost], "a session that isn't there is named, not an error");
        }
        other => panic!("{other:?}"),
    }
    // adding a second only sends the second: the first is already on screen.
    match follow(vec![a.id, b.id]).await.unwrap() {
        Response::Followed { snapshots, .. } => assert_eq!(snapshots.iter().map(|s| s.summary.id).collect::<Vec<_>>(), vec![b.id]),
        other => panic!("{other:?}"),
    }

    for s in [a.id, b.id] {
        c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s, text: "hello".into(), attachments: vec![] }).await.unwrap();
    }
    let mut heard = std::collections::HashSet::new();
    events_until(&mut ev, "items from both chats", |e| {
        if let Event::ItemUpserted { session, .. } = e {
            heard.insert(*session);
        }
        heard.len() == 2
    })
    .await;

    // following only b: a goes quiet.
    follow(vec![b.id]).await.unwrap();
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: a.id, text: "again".into(), attachments: vec![] }).await.unwrap();
    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: b.id, text: "again".into(), attachments: vec![] }).await.unwrap();
    let seen = events_until(&mut ev, "an item from b", |e| matches!(e, Event::ItemUpserted { session, .. } if *session == b.id)).await;
    assert!(!seen.iter().any(|e| matches!(e, Event::ItemUpserted { session, .. } if *session == a.id)), "unfollowed: no items from a");
}

#[tokio::test]
async fn a_new_workspace_gets_its_files_its_setup_its_ports_and_its_buttons() {
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("app");
    std::fs::create_dir_all(repo.join(".tessel")).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    std::fs::write(repo.join(".env"), "SECRET=1\n").unwrap();
    std::fs::write(
        repo.join(".tessel/settings.toml"),
        "[scripts]\nsetup = \"echo set up > ready.txt\"\n[scripts.run.dev]\ncommand = \"npm run dev -- --port $TESSEL_PORT\"\n",
    )
    .unwrap();
    git(&repo, &["add", "-A"]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    let (c, _) = d.client().await;
    let create = Request::SessionCreate {
        cmd_id: Uuid::new_v4(), cwd: repo.to_string_lossy().into_owned(), worktree: Some(WorktreeSpec { base: None, name: Some("ws".into()) }),
        goal: Some("w".into()), prompt: Some("hi".into()), model: None, parent: None, attachments: vec![],
    };
    let Response::Session { summary: s } = c.request(create).await.unwrap() else { panic!() };
    let ws = PathBuf::from(&s.cwd);
    assert_eq!(std::fs::read_to_string(ws.join(".env")).unwrap(), "SECRET=1\n", "the checkout's ignored .env, copied in");

    // its setup ran, in a terminal of the session's.
    let Response::Terms { terms } = c.request(Request::TermList).await.unwrap() else { panic!() };
    assert!(terms.iter().any(|t| t.session == Some(s.id) && t.command == ["echo set up > ready.txt"]), "{terms:?}");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !ws.join("ready.txt").exists() {
        assert!(tokio::time::Instant::now() < deadline, "setup never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // its buttons and its ports.
    let Response::Scripts { scripts, port } = c.request(Request::Scripts { session: s.id }).await.unwrap() else { panic!() };
    assert_eq!(scripts.actions.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(), ["dev"]);
    assert!(scripts.from.last().is_some_and(|f| f.ends_with(".tessel/settings.toml")), "{:?}", scripts.from);
    assert!(port >= 41000 && port % 10 == 0);
    // the agent is told them too.
    until(&c, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    let env = d.backend.spawned.lock().unwrap()[0].env.clone();
    assert!(env.contains(&("TESSEL_PORT".into(), port.to_string())) && env.contains(&("TESSEL_WORKSPACE_PATH".into(), s.cwd.clone())), "{env:?}");
}

#[tokio::test]
async fn a_restart_hands_over_the_lock_so_nothing_can_start_in_between() {
    use std::os::fd::FromRawFd;
    let tmp = tempfile::Builder::new().prefix("ph").tempdir_in("/tmp").unwrap();
    let paths = Paths::at(tmp.path());
    // the old daemon's lock, and a copy of it, as a restart sends it.
    let old = tesseld::lock_instance(&paths).unwrap();
    let copy = unsafe { std::os::fd::OwnedFd::from_raw_fd(libc::dup(old.fd())) };
    let manifest = tesseld::handoff::Manifest { lock: true, ..Default::default() };
    let backend = Arc::new(ReplayBackend::new(vec![fixture("plain")]));
    // the new daemon starts at once, though the old one hasn't let go yet.
    let started = tokio::time::timeout(Duration::from_secs(2), tesseld::start_with(Config::new(paths.clone()), backend, Some((manifest, vec![copy])))).await;
    let running = started.expect("it didn't wait for the old daemon to leave").unwrap();
    // and at no moment can a third take over: not now, nor once the old one is gone.
    assert!(tesseld::lock_instance(&paths).is_err());
    drop(old);
    assert!(tesseld::lock_instance(&paths).is_err(), "the new daemon holds it");
    running.hub.send(HubMsg::Stop).ok();
    let _ = tokio::time::timeout(Duration::from_secs(5), running.done).await;
}

#[tokio::test]
async fn a_request_after_the_daemon_has_gone_is_refused_at_once_not_left_waiting() {
    let mut d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    c.request(Request::SessionList).await.unwrap();
    d.stop().await;
    // the connection's end may take a moment to be noticed; after that, nothing waits.
    tokio::time::sleep(Duration::from_millis(100)).await;
    for _ in 0..3 {
        let answer = tokio::time::timeout(Duration::from_secs(2), c.request(Request::SessionList)).await;
        assert!(matches!(answer, Ok(Err(_))), "refused at once, not waited on: {answer:?}");
    }
    assert!(c.is_closed(), "and it says so, for whoever holds it to connect again");
}

#[tokio::test]
async fn waiting_for_text_a_finished_program_never_showed_ends_at_once() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let Response::Term { info } = c.request(Request::TermOpen { session: None, cwd: Some(d.cwd()), command: vec!["echo Dev server running".into()], cols: 80, rows: 24, agent: false }).await.unwrap() else { panic!() };
    let started = std::time::Instant::now();
    let waited = c.request(Request::TermWait { term: info.id, text: Some("Local:".into()), timeout_ms: 30_000 }).await;
    let err = waited.unwrap_err().to_string();
    assert!(started.elapsed() < Duration::from_secs(5), "not the whole 30s");
    assert!(err.contains("without showing") && err.contains("Dev server running"), "says so, and what it shows instead: {err}");
}

#[tokio::test]
async fn a_short_dictation_is_tidied_without_a_model() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _ev) = d.client().await;
    let Response::Tidied { text, by } = c.request(Request::VoiceTidy { session: None, text: "um, ship it.".into() }).await.unwrap() else { panic!() };
    assert_eq!((text.as_str(), by.as_str()), ("Ship it.", "filter"), "a couple of words: the noises go, nothing more is asked");
}

#[tokio::test]
async fn signing_in_again_moves_the_agents_onto_the_new_sign_in() {
    let d = Daemon::start(&["plain", "resume"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hi")).await;
    let done = until(&c, s.id, "a warm agent, between turns", |s| s.warm && wants(s) == Some(AttentionReason::Finished)).await;

    // a new token, brought from a mac (or pasted): the agent is let go, its chat kept.
    let remote = d.cfg.paths.home.join("remote");
    std::fs::create_dir_all(&remote).unwrap();
    std::fs::write(remote.join("claude-token"), "sk-ant-oat01-another-account").unwrap();
    until(&c, s.id, "let go", |s| !s.warm).await;

    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "still there?".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "second turn", |s| s.last_seq > done.last_seq && wants(s) == Some(AttentionReason::Finished)).await;
    let spawned = d.backend.spawned.lock().unwrap().clone();
    assert_eq!(spawned.len(), 2);
    assert!(spawned[1].resume, "the conversation carries on");
    assert!(spawned[1].env.iter().any(|(k, v)| k == "CLAUDE_CODE_OAUTH_TOKEN" && v == "sk-ant-oat01-another-account"), "on the new sign-in");
}

#[tokio::test]
async fn deleted_chats_are_kept_thirty_days() {
    let mut d = Daemon::start(&["plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), None).await;
    let t = create(&c, &d.cwd(), None).await;
    for id in [s.id, t.id] {
        c.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session: id, remove_worktree: false }).await.unwrap();
    }
    d.stop().await;
    let old = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64 - tessel_proto::KEEP_DELETED_MS - 60_000) as i64;
    rusqlite::Connection::open(d.cfg.paths.db()).unwrap().execute("UPDATE deleted SET deleted_ms = ?1 WHERE id = ?2", rusqlite::params![old, s.id.to_string()]).unwrap();
    d.boot(&["plain"]).await;
    let (c, _) = d.client().await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while deleted(&c).await.len() != 1 {
        assert!(tokio::time::Instant::now() < deadline, "a month-old one wasn't let go");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(deleted(&c).await[0].id, t.id, "the recent one stays");
}


fn write_workflow(d: &Daemon, file: &str, yaml: &str) {
    let dir = d.cfg.paths.home.join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(file), yaml).unwrap();
}

async fn run_of(c: &Client, head: SessionId, what: &str, pred: impl Fn(&tessel_proto::Run) -> bool) -> tessel_proto::Run {
    until(c, head, what, |s| s.run.as_ref().is_some_and(&pred)).await.run.unwrap()
}

fn pick(value: &str, label: &str) -> tessel_proto::Pick {
    tessel_proto::Pick { value: serde_json::Value::String(value.into()), label: label.into() }
}

#[tokio::test]
async fn a_workflow_runs_its_steps_in_order_and_keeps_its_doc() {
    let d = Daemon::start(&["plain+"]).await;
    let folder = d.cfg.paths.home.join("out");
    write_workflow(&d, "notes.yaml", &format!(r#"
name: Notes
title: "Notes on {{{{topic.label}}}}"
folder: {}
inputs:
  topic: {{ label: Topic, type: select, options: [{{ value: lim, label: Limits }}] }}
steps:
  - id: gather
    prompt: Gather what you know about {{{{topic.label}}}} ({{{{topic}}}}).
  - shell: echo checked > checked.txt
  - prompt: "Write the notes from: {{{{steps.gather.text}}}}"
    fresh: true
    doc: true
"#, folder.display()));
    let (c, _) = d.client().await;
    let Response::Workflows { workflows } = c.request(Request::Workflows { cwd: None }).await.unwrap() else { panic!() };
    assert_eq!((workflows.len(), workflows[0].problems.len()), (1, 0), "{:?}", workflows[0].problems);
    let Response::Choices { choices } = c.request(Request::WorkflowOptions { workflow: "notes".into(), input: "topic".into(), picked: Default::default(), cwd: None, fresh: false }).await.unwrap() else { panic!() };
    assert_eq!(choices[0].label, "Limits");

    match c.request(Request::WorkflowRun { workflow: "notes".into(), picked: Default::default(), cwd: None }).await {
        Err(tessel_client::ClientError::Rpc(e)) => assert!(e.message.contains("needs Topic"), "{}", e.message),
        other => panic!("a missing input is refused: {other:?}"),
    }
    let picked = [("topic".to_owned(), pick("lim", "Limits"))].into_iter().collect();
    let Response::Session { summary: head } = c.request(Request::WorkflowRun { workflow: "notes".into(), picked, cwd: None }).await.unwrap() else { panic!() };
    assert_eq!(head.goal, "Notes on Limits");
    assert_eq!(head.cwd, folder.to_string_lossy());

    let run = run_of(&c, head.id, "the run to finish", |r| r.done()).await;
    assert_eq!(run.failed, None);
    assert!(run.steps.iter().all(|s| s.state == tessel_proto::StepState::Done), "{:?}", run.steps);
    assert_eq!(std::fs::read_to_string(folder.join("checked.txt")).unwrap().trim(), "checked", "the shell step ran in the run's folder");
    let doc = run.doc.expect("a doc");
    assert!(!std::fs::read_to_string(&doc).unwrap().trim().is_empty());
    assert_ne!(run.steps[2].session, Some(head.id), "the fresh step has a tab of its own");
    let tab = list(&c).await.into_iter().find(|s| Some(s.id) == run.steps[2].session).expect("the fresh tab");
    assert_eq!((tab.cwd.as_str(), tab.parent), (head.cwd.as_str(), Some(head.id)));

    let written = d.backend.written.lock().unwrap().join("\n");
    assert!(written.contains("Gather what you know about Limits (lim)."), "the step's prompt, filled in");
    assert!(written.contains("What was done so far"), "the fresh step is told what came before");
    assert!(written.contains("final message and nothing else"), "the doc step is asked for the doc itself");
}

#[tokio::test]
async fn an_ask_step_waits_for_your_answer_before_going_on() {
    let d = Daemon::start(&["plain+"]).await;
    write_workflow(&d, "chat.yaml", "name: Chat\nsteps:\n  - ask: What should I focus on?\n  - prompt: Go on with that.\n");
    let (c, _) = d.client().await;
    let Response::Session { summary: head } = c.request(Request::WorkflowRun { workflow: "chat".into(), picked: Default::default(), cwd: None }).await.unwrap() else { panic!() };
    let asking = run_of(&c, head.id, "the question to be asked", |r| r.steps[0].state == tessel_proto::StepState::Asking).await;
    assert_eq!(asking.at, 0);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let still = until(&c, head.id, "still asking", |_| true).await.run.unwrap();
    assert_eq!(still.steps[1].state, tessel_proto::StepState::Waiting, "nothing goes on until you answer");

    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: head.id, text: "limits, mostly".into(), attachments: vec![] }).await.unwrap();
    let run = run_of(&c, head.id, "the run to finish", |r| r.done()).await;
    assert!(run.steps.iter().all(|s| s.state == tessel_proto::StepState::Done), "{:?}", run.steps);
}

#[tokio::test]
async fn an_agent_can_ask_with_a_form_and_wait_for_the_answer() {
    let d = Daemon::start(&["plain"]).await;
    let (agent, _) = d.client().await;
    let (window, _) = d.client().await;
    let s = create(&agent, &d.cwd(), None).await;
    let bad = agent.request(Request::Ask { session: s.id, form: serde_json::json!({"fields": [{"id": "x", "label": "X", "type": "select"}]}) }).await;
    assert!(matches!(bad, Err(tessel_client::ClientError::Rpc(e)) if e.message.contains("needs options")));

    let form = serde_json::json!({"title": "Which one?", "fields": [{"id": "pick", "label": "Pick", "type": "select", "options": ["a", "b"]}, {"id": "why", "label": "Why", "type": "long", "optional": true}]});
    let agent = std::sync::Arc::new(agent);
    let asking = tokio::spawn({
        let agent = agent.clone();
        let id = s.id;
        async move { agent.request(Request::Ask { session: id, form }).await }
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let surface = loop {
        let snap = snapshot(&window, s.id).await;
        if let Some(name) = snap.items.iter().find_map(|i| match &i.body {
            ItemBody::Surface { surface, node, .. } if node["k"] == "form" => Some(surface.clone()),
            _ => None,
        }) {
            break name;
        }
        assert!(tokio::time::Instant::now() < deadline, "the form never showed");
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    assert!(!asking.is_finished(), "the agent waits");
    window.request(Request::AskAnswer { session: s.id, surface: surface.clone(), answers: serde_json::json!({"pick": "b"}) }).await.unwrap();
    let Ok(Ok(Response::Answered { answers })) = asking.await else { panic!("the agent hears the answer") };
    assert_eq!(answers, serde_json::json!({"pick": "b"}));
    let snap = snapshot(&window, s.id).await;
    let answered = snap.items.iter().find_map(|i| match &i.body {
        ItemBody::Surface { surface: n, node, .. } if *n == surface => Some(node.clone()),
        _ => None,
    });
    assert_eq!(answered.unwrap()["p"]["answered"], true, "the form shows it was answered");
    let again = window.request(Request::AskAnswer { session: s.id, surface, answers: serde_json::json!({}) }).await;
    assert!(matches!(again, Err(tessel_client::ClientError::Rpc(e)) if e.message.contains("isn't waiting")));
}

#[tokio::test]
async fn visual_mode_shows_the_agents_commands_even_when_it_was_turned_on_mid_turn() {
    let d = Daemon::start(&["perm_allow"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("please write the file")).await;
    until(&c, s.id, "needs permission", |s| s.run_state == RunState::NeedsPermission).await;
    c.request(Request::SessionSetVisual { session: s.id, visual: true }).await.unwrap();
    let snap = snapshot(&c, s.id).await;
    let answer = Request::PermissionRespond { cmd_id: Uuid::new_v4(), session: s.id, request_id: snap.pending[0].request_id.clone(), decision: PermissionDecision::Allow { updated_input: None } };
    c.request(answer).await.unwrap();
    until(&c, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;

    let Response::Terms { terms } = c.request(Request::TermList).await.unwrap() else { panic!() };
    let mirror = terms.iter().find(|t| t.agent && t.session == Some(s.id)).expect("a terminal showing its commands");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let Response::TermText { text } = c.request(Request::TermCapture { term: mirror.id, history: true, format: None }).await.unwrap() else { panic!() };
        if text.contains("echo tessel-fixture > out.txt") {
            assert!(text.contains("Write tessel-fixture text into out.txt"), "headed by what it's for: {text}");
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "the command never showed: {text:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_chat_gets_a_short_name_unless_you_gave_it_one() {
    let d = Daemon::start_with(&["plain"], |cfg| cfg.titles = true).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hey can you look at the open prs and tell me if they're good to merge")).await;
    let named = until(&c, s.id, "named", |s| s.goal != "hey can you look at the open prs and tell me if they're good to merge" && !s.goal.is_empty()).await;
    assert_eq!(named.goal, "fix: what the replay agent would say");
    let asked = d.backend.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|(q, m)| m == "haiku" && q.contains("open prs")), "{asked:?}");

    let Response::Session { summary: mine } = c
        .request(Request::SessionCreate { cmd_id: Uuid::new_v4(), cwd: d.cwd(), worktree: None, goal: Some("Mine".into()), prompt: Some("do it".into()), model: None, parent: None, attachments: vec![] })
        .await
        .unwrap()
    else {
        panic!()
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(list(&c).await.into_iter().find(|x| x.id == mine.id).unwrap().goal, "Mine", "a name you gave stays");
}

#[tokio::test]
async fn a_picture_the_agent_showed_can_be_fetched_and_nothing_else() {
    let d = Daemon::start(&["plain"]).await;
    let (c, _) = d.client().await;
    let s = create(&c, &d.cwd(), Some("hi")).await;
    until(&c, s.id, "finished", |s| wants(s) == Some(AttentionReason::Finished)).await;
    let not_said = c.request(Request::ChatImage { session: s.id, path: "/etc/hosts.png".into() }).await;
    assert!(matches!(not_said, Err(tessel_client::ClientError::Rpc(e)) if e.message.contains("only a picture")));
    let not_picture = c.request(Request::ChatImage { session: s.id, path: "/etc/hosts".into() }).await;
    assert!(not_picture.is_err());
}

#[tokio::test]
async fn a_change_is_proved_against_the_house_rules_and_a_failed_proof_holds_the_ship() {
    use tessel_proto::proof::{Severity, Verdict};
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(repo.join("src/k8s")).unwrap();
    std::fs::create_dir_all(repo.join(".tessel/rules")).unwrap();
    std::fs::write(repo.join("src/k8s/deploy.go"), "package k8s\n").unwrap();
    std::fs::write(repo.join(".tessel/rules/k8s.md"), "---\npaths: \"**/k8s/**\"\n---\nKubernetes objects change only through the reconciler.\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nverify = \"true\"\nlenses = [\"rules\", \"correctness\"]\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    d.backend.reviews.lock().unwrap().extend([
        ("one thing only: rules".to_owned(), "```json\n[{\"file\":\"src/k8s/deploy.go\",\"line\":2,\"severity\":\"block\",\"rule\":\"k8s\",\"title\":\"Updates the object directly\",\"why\":\"client.Update skips the reconciler\"}]\n```".to_owned()),
        ("one thing only: correctness".to_owned(), "```json\n[{\"file\":\"src/k8s/deploy.go\",\"line\":2,\"severity\":\"warn\",\"title\":\"Imagined problem\",\"why\":\"maybe\"}]\n```".to_owned()),
        ("Other reviewers reported".to_owned(), "```json\n[{\"n\":0,\"real\":true},{\"n\":1,\"real\":false}]\n```".to_owned()),
    ]);
    let (c, _) = d.client().await;
    let req = Request::SessionCreate {
        cmd_id: Uuid::new_v4(), cwd: repo.to_string_lossy().into_owned(), worktree: Some(WorktreeSpec { base: None, name: Some("direct".into()) }),
        goal: Some("make deploys faster".into()), prompt: None, model: None, parent: None, attachments: vec![],
    };
    let Response::Session { summary: s } = c.request(req).await.unwrap() else { panic!() };
    let ws = PathBuf::from(&s.cwd);
    std::fs::write(ws.join("src/k8s/deploy.go"), "package k8s\nfunc Deploy() { client.Update(obj) }\n").unwrap();

    c.request(Request::ProofRun { session: s.id, anyway: false }).await.unwrap();
    let done = until(&c, s.id, "proved", |s| s.proof.as_ref().is_some_and(|p| p.verdict.done())).await;
    let p = done.proof.unwrap();
    assert_eq!(p.verdict, Verdict::Failed);
    assert_eq!(p.rules.iter().filter(|r| *r == "k8s").count(), 1, "the rule for those paths applied");
    assert!(p.verify.iter().all(|v| v.ok));
    let k8s = p.findings.iter().find(|f| f.rule.as_deref() == Some("k8s")).expect("the rule's finding");
    assert_eq!((k8s.severity.clone(), k8s.confirmed, k8s.file.as_deref(), k8s.line), (Severity::Block, Some(true), Some("src/k8s/deploy.go"), Some(2)));
    assert_eq!(p.findings.iter().find(|f| f.title == "Imagined problem").unwrap().confirmed, Some(false), "refuted on a second look");
    let asked = d.backend.asked.lock().unwrap().clone();
    assert!(asked.iter().any(|(q, _)| q.contains("one thing only: rules") && q.contains("reconciler") && q.contains("client.Update")), "the reviewer sees the rule and the change");

    let ship = |force| Request::Ship { session: s.id, step: ShipStep::Push, force };
    match c.request(ship(false)).await {
        Err(tessel_client::ClientError::Rpc(e)) => assert!(e.code == ErrorCode::Conflict && e.message.starts_with("proof failed"), "{e:?}"),
        other => panic!("shipped past a failed proof: {other:?}"),
    }
    let forced = c.request(ship(true)).await;
    assert!(!matches!(&forced, Err(tessel_client::ClientError::Rpc(e)) if e.message.starts_with("proof failed")), "ship anyway goes past it");
    until(&c, s.id, "the override is kept", |s| s.proof.as_ref().is_some_and(|p| p.overridden)).await;

    std::fs::write(ws.join("src/k8s/deploy.go"), "package k8s\nfunc Deploy() { reconcile(obj) }\n").unwrap();
    let changed = c.request(ship(false)).await;
    assert!(!matches!(&changed, Err(tessel_client::ClientError::Rpc(e)) if e.message.starts_with("proof failed")), "a proof of different code doesn't hold this one");
}

#[tokio::test]
async fn a_failing_check_stops_a_proof_before_any_reviewer_is_paid_for() {
    use tessel_proto::proof::Verdict;
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(repo.join(".tessel")).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nverify = \"echo 'error: it broke' && false\"\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    let (c, _) = d.client().await;
    let s = create(&c, &repo.to_string_lossy(), None).await;
    c.request(Request::ProofRun { session: s.id, anyway: false }).await.unwrap();
    let p = until(&c, s.id, "proved", |s| s.proof.as_ref().is_some_and(|p| p.verdict.done())).await.proof.unwrap();
    assert_eq!(p.verdict, Verdict::Failed);
    assert!(p.verify[0].output.contains("error: it broke"));
    assert!(p.reviewers.is_empty() && p.cost_usd == 0.0);
    assert!(!d.backend.asked.lock().unwrap().iter().any(|(q, _)| q.contains("reviewing a change")), "no reviewer was asked");
}

#[tokio::test]
async fn a_repository_that_opts_in_is_proved_after_each_turn_but_never_twice_for_the_same_code() {
    let d = Daemon::start(&["plain+"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(repo.join(".tessel")).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nauto = true\nlenses = [\"correctness\"]\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    let (c, _) = d.client().await;
    let s = create(&c, &repo.to_string_lossy(), Some("change it")).await;
    let first = until(&c, s.id, "proved by itself", |s| s.proof.as_ref().is_some_and(|p| p.verdict.done())).await.proof.unwrap();
    let reviews = || d.backend.asked.lock().unwrap().iter().filter(|(q, _)| q.contains("reviewing a change")).count();
    assert_eq!(reviews(), 1);

    c.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session: s.id, text: "anything else?".into(), attachments: vec![] }).await.unwrap();
    until(&c, s.id, "the second turn ended", |s| s.run_state == RunState::Idle && s.last_seq > 0).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(reviews(), 1, "nothing changed, so nothing is paid for again");
    assert_eq!(list(&c).await.iter().find(|x| x.id == s.id).unwrap().proof.as_ref().unwrap().started_ms, first.started_ms);
}

async fn task_until(c: &Client, id: &str, what: &str, pred: impl Fn(&tessel_proto::task::Task) -> bool) -> tessel_proto::task::Task {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let Response::Tasks { tasks } = c.request(Request::TaskList).await.unwrap() else { panic!() };
        let t = tasks.into_iter().find(|t| t.id == id);
        if let Some(t) = &t
            && pred(t)
        {
            return t.clone();
        }
        assert!(tokio::time::Instant::now() < deadline, "timed out waiting for: {what}. last: {t:?}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn a_task_runs_in_its_own_workspace_is_proved_sent_back_and_landed_in_order() {
    use tessel_proto::task::{TaskAct, TaskState};
    let d = Daemon::start(&["plain+"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(repo.join(".tessel")).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nverify = \"test ! -f broken\"\nlenses = [\"correctness\"]\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    let (c, _) = d.client().await;
    let make = |title: &str, deps: Vec<String>| Request::TaskCreate { repo: repo.to_string_lossy().into_owned(), title: title.into(), spec: "keep it small".into(), deps, parent: None, ready: true, budget_usd: None, plan: false, from: None, attachments: vec![] };
    let Response::Task { task: first } = c.request(make("Count higher", vec![])).await.unwrap() else { panic!() };
    let Response::Task { task: second } = c.request(make("Then count lower", vec![first.id.clone()])).await.unwrap() else { panic!() };

    let first = task_until(&c, &first.id, "worked and proved", |t| t.state == TaskState::Review).await;
    let s = first.session.expect("it got a session");
    let summary = list(&c).await.into_iter().find(|x| x.id == s).unwrap();
    assert!(summary.worktree.is_some(), "in a workspace of its own");
    assert!(d.backend.written.lock().unwrap().iter().any(|l| l.contains("Count higher") && l.contains("keep it small")), "the agent got the task");
    assert!(task_until(&c, &second.id, "waits", |t| t.state == TaskState::Ready).await.session.is_none(), "it waits for the one it depends on");

    let ws = PathBuf::from(&summary.cwd);
    std::fs::write(ws.join("broken"), "x\n").unwrap();
    c.request(Request::TaskAct { id: first.id.clone(), act: TaskAct::Change { text: "also count to three".into() } }).await.unwrap();
    let sent_back = task_until(&c, &first.id, "sent back twice, then yours", |t| t.state == TaskState::Review && t.attempts == 2).await;
    assert!(sent_back.notes.iter().any(|n| n.text.contains("still failing after 2 fix rounds")));
    assert_eq!(d.backend.written.lock().unwrap().iter().filter(|l| l.contains("Checking your change found problems")).count(), 2);

    std::fs::remove_file(ws.join("broken")).unwrap();
    std::fs::write(ws.join("a.txt"), "two\n").unwrap();
    git(&ws, &["commit", "-q", "-am", "fix: count higher"]);
    c.request(Request::TaskAct { id: first.id.clone(), act: TaskAct::Land }).await.unwrap();
    let landed = task_until(&c, &first.id, "landed", |t| t.state.finished()).await;
    assert_eq!(landed.state, TaskState::Landed, "{:?}", landed.notes);
    assert_eq!(git(&repo, &["log", "-1", "--format=%s"]), "fix: count higher");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "two\n");
    assert!(task_until(&c, &second.id, "now it starts", |t| t.session.is_some()).await.state != TaskState::Ready);
}

#[tokio::test]
async fn agents_get_placeholders_never_secrets_and_a_sealed_repository_seals_them() {
    let d = Daemon::start(&["plain+"]).await;
    let (c, _) = d.client().await;
    let key = tessel_proto::keys::KeyInfo { name: "deploy".into(), hosts: vec!["deploy.example.com".into()], header: "authorization".into(), format: "Bearer {key}".into(), env: Some("DEPLOY_TOKEN".into()), ..Default::default() };
    c.request(Request::KeySet { key, value: None }).await.unwrap();
    let open = create(&c, &d.cwd(), Some("hi")).await;
    until(&c, open.id, "ran", |s| wants(s) == Some(AttentionReason::Finished)).await;
    let env = d.backend.spawned.lock().unwrap()[0].env.clone();
    assert!(env.iter().any(|(k, v)| k == "DEPLOY_TOKEN" && v == "tessel-key-deploy"), "{env:?}");
    assert!(!env.iter().any(|(k, _)| k == "HTTPS_PROXY"), "no secret here, so nothing to add on the way out: no proxy");
    assert!(d.backend.spawned.lock().unwrap()[0].sandbox.is_none());

    let repo = d.cfg.paths.home.join("sealed");
    std::fs::create_dir_all(repo.join(".tessel")).unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[sandbox]\nsealed = true\nallow = [\"registry.npmjs.org\"]\n").unwrap();
    let sealed = create(&c, &repo.to_string_lossy(), Some("hi")).await;
    until(&c, sealed.id, "ran", |s| wants(s) == Some(AttentionReason::Finished)).await;
    let spawned = d.backend.spawned.lock().unwrap()[1].clone();
    let proxy = spawned.env.iter().find(|(k, _)| k == "HTTPS_PROXY").map(|(_, v)| v.clone()).expect("sealed agents go through tessel");
    assert!(proxy.starts_with("http://tessel:") && proxy.contains("@127.0.0.1:"));
    assert!(spawned.env.iter().any(|(k, v)| k == "NO_PROXY" && !v.contains("anthropic")), "even its model goes through the allow list");
    if cfg!(target_os = "macos") {
        let profile = spawned.sandbox.expect("sealed by macOS");
        assert!(profile.contains("(deny network*)") && profile.contains(&*std::fs::canonicalize(&repo).unwrap().to_string_lossy()));
    }
}

fn workflow_file(d: &Daemon, name: &str, yaml: &str) {
    let dir = d.cfg.paths.home.join("workflows");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
}

#[tokio::test]
async fn a_schedule_runs_a_workflow_once_a_minute_and_a_file_change_runs_one_once() {
    let d = Daemon::start_with(&["plain+"], |cfg| cfg.watch_every = Duration::from_millis(30)).await;
    let notes = d.cfg.paths.home.join("notes");
    std::fs::create_dir_all(&notes).unwrap();
    std::fs::write(notes.join("a.md"), "a").unwrap();
    workflow_file(&d, "minutely", "name: Minutely\non: {cron: \"* * * * *\"}\nsteps:\n  - \"Say hi, it's {{event.title}}\"\n");
    workflow_file(&d, &"watch", &format!("name: Watch\non:\n  files: {{in: \"{}\", paths: [\"*.md\"]}}\nsteps:\n  - \"These changed: {{{{event.files}}}}\"\n", notes.display()));
    let (c, _) = d.client().await;
    let wrote = |needle: &str| d.backend.written.lock().unwrap().iter().filter(|l| l.contains(needle)).count();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while wrote("Say hi, it's Minutely at") == 0 {
        assert!(tokio::time::Instant::now() < deadline, "the schedule never ran");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wrote("Say hi, it's Minutely at"), 1, "once in its minute, however often it's checked");

    tokio::time::sleep(Duration::from_millis(200)).await;
    std::fs::write(notes.join("b.md"), "b").unwrap();
    std::fs::write(notes.join("ignored.txt"), "x").unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while wrote("These changed: b.md") == 0 {
        assert!(tokio::time::Instant::now() < deadline, "the change never ran it: {:?}", d.backend.written.lock().unwrap().iter().filter(|l| l.contains("changed")).collect::<Vec<_>>());
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(wrote("These changed:"), 1);
    let Response::Triggers { triggers } = c.request(Request::Triggers).await.unwrap() else { panic!() };
    assert!(triggers.iter().any(|t| t.name == "Watch" && t.last.as_deref() == Some("ran")));
}

#[tokio::test]
async fn a_triggered_workflow_changes_code_only_by_filing_a_task() {
    use tessel_proto::task::TaskState;
    let d = Daemon::start(&["plain+"]).await;
    let repo = d.cfg.paths.home.join("web");
    std::fs::create_dir_all(&repo).unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["commit", "-q", "--allow-empty", "-m", "feat: start"]] {
        git(&repo, args);
    }
    workflow_file(&d, "triage", &format!("name: Triage\non: {{github: {{repo: acme/web, label: agent}}}}\nsteps:\n  - task: \"Fix {{{{event.title}}}}\"\n    repo: \"{}\"\n    spec: \"From {{{{event.url}}}}: {{{{event.body}}}}\"\n", repo.display()));
    workflow_file(&d, "broken", "name: Broken\non: {cron: \"0 9 * * *\"}\ninputs:\n  who: {label: Who}\nsteps: [\"hi {{who}}\"]\n");
    let (c, _) = d.client().await;
    let event = serde_json::json!({"title": "the login bug", "url": "https://github.com/acme/web/issues/7", "body": "it 500s"});
    c.request(Request::TriggerFire { workflow: "triage".into(), event, cwd: None }).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let task = loop {
        let Response::Tasks { tasks } = c.request(Request::TaskList).await.unwrap() else { panic!() };
        if let Some(t) = tasks.into_iter().find(|t| t.title == "Fix the login bug") {
            break t;
        }
        assert!(tokio::time::Instant::now() < deadline, "no task was filed");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert!(task.spec.contains("issues/7") && task.spec.contains("it 500s"));
    assert_ne!(task.state, TaskState::Draft, "ready to be picked up");
    let Response::Workflows { workflows } = c.request(Request::Workflows { cwd: None }).await.unwrap() else { panic!() };
    let broken = workflows.iter().find(|w| w.name == "Broken").unwrap();
    assert!(broken.problems.iter().any(|p| p.contains("\"who\" needs a default")), "{:?}", broken.problems);
}

#[tokio::test]
async fn a_big_task_is_planned_into_small_ones_and_lands_when_they_all_have() {
    use tessel_proto::task::{TaskAct, TaskState};
    let d = Daemon::start(&["plain+"]).await;
    let repo = d.cfg.paths.home.join("app");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    std::fs::write(repo.join("b.txt"), "b\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    let (c, _) = d.client().await;
    let Response::Task { task: big } = c.request(Request::TaskCreate { repo: repo.to_string_lossy().into_owned(), title: "Add billing".into(), spec: "Stripe".into(), deps: vec![], parent: None, ready: true, budget_usd: None, plan: true, from: None, attachments: vec![] }).await.unwrap() else { panic!() };
    let reviewed = task_until(&c, &big.id, "the planner ran", |t| t.state == TaskState::Review).await;
    let planner = reviewed.session.expect("a planner chat");
    let summary = list(&c).await.into_iter().find(|s| s.id == planner).unwrap();
    assert!(summary.worktree.is_none(), "planning needs no workspace of its own: {:?} {}", summary.worktree, summary.cwd);
    assert!(d.backend.written.lock().unwrap().iter().any(|l| l.contains("Plan this task, don't do it") && l.contains("Add billing")));
    assert!(reviewed.notes.iter().any(|n| n.text.contains("filed no tasks")));

    let file = |title: &str, after: Vec<String>| Request::TaskCreate { repo: "/ignored".into(), title: title.into(), spec: String::new(), deps: after, parent: None, ready: false, budget_usd: None, plan: false, from: Some(planner), attachments: vec![] };
    let Response::Task { task: one } = c.request(file("Add the plans table", vec![])).await.unwrap() else { panic!() };
    let Response::Task { task: two } = c.request(file("Charge monthly", vec![one.id.clone()])).await.unwrap() else { panic!() };
    assert_eq!((one.parent.as_deref(), one.repo.clone()), (Some(big.id.as_str()), big.repo.clone()), "the planner's tasks are the big one's, in its repository");
    assert_ne!(two.state, TaskState::Draft, "ready, as the planner filed them");
    assert_eq!(task_until(&c, &big.id, "waiting on its tasks", |t| t.state == TaskState::Planned).await.state, TaskState::Planned);

    for (t, f) in [(&one, "a.txt"), (&two, "b.txt")] {
        let t = task_until(&c, &t.id, "worked", |x| x.state == TaskState::Review).await;
        let ws = PathBuf::from(list(&c).await.into_iter().find(|s| Some(s.id) == t.session).unwrap().cwd);
        std::fs::write(ws.join(f), format!("{} changed\n", t.title)).unwrap();
        git(&ws, &["commit", "-q", "-am", &format!("feat: {}", t.title.to_lowercase())]);
        c.request(Request::TaskAct { id: t.id.clone(), act: TaskAct::Land }).await.unwrap();
        let landed = task_until(&c, &t.id, "landed", |x| x.state.finished()).await;
        assert_eq!(landed.state, TaskState::Landed, "{:?}", landed.notes);
    }
    let done = task_until(&c, &big.id, "done when they all are", |t| t.state.finished()).await;
    assert_eq!(done.state, TaskState::Landed);
    assert!(done.notes.iter().any(|n| n.text == "all 2 of its tasks landed"));
    assert_eq!(git(&repo, &["log", "--format=%s", "-n", "2"]), "feat: charge monthly\nfeat: add the plans table");
}

#[tokio::test]
async fn a_change_is_looked_at_and_tried_like_a_user_before_any_reviewer_reads_it() {
    use tessel_proto::proof::{Severity, Verdict};
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("shop");
    for dir in [".tessel/rules", ".tessel/scenarios"] {
        std::fs::create_dir_all(repo.join(dir)).unwrap();
    }
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nlook = \"printf 'png' > \\\"$TESSEL_SHOTS/board.png\\\"\"\nlenses = [\"correctness\"]\n").unwrap();
    std::fs::write(repo.join(".tessel/rules/roomy.md"), "---\ncheck: look\nseverity: warn\n---\nControls need room to breathe.\n").unwrap();
    std::fs::write(repo.join(".tessel/scenarios/smoke.md"), "---\nrun: test -f a.txt\n---\n").unwrap();
    std::fs::write(repo.join(".tessel/scenarios/sign-in.md"), "---\npaths: [\"a.txt\"]\n---\n# Sign in\nSign in and see the board.\n").unwrap();
    std::fs::write(repo.join(".tessel/scenarios/search.md"), "# Search\nSearch for a product.\n").unwrap();
    let hidden = d.cfg.paths.home.join("holdouts/shop/scenarios");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(hidden.join("pay.md"), "---\npaths: [\"a.txt\"]\n---\n# Pay\nBuy something and pay.\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    std::fs::write(repo.join("a.txt"), "two\n").unwrap();
    d.backend.reviews.lock().unwrap().extend([
        ("judge how it looks".to_owned(), "```json\n[{\"lens\":\"look\",\"file\":\"board.png\",\"severity\":\"warn\",\"rule\":\"roomy\",\"title\":\"Buttons touch the card's edge\",\"why\":\"bottom right\"}]\n```".to_owned()),
        ("The scenario: Sign in".to_owned(), "Signed in.\n```json\n{\"passed\": true, \"said\": \"saw the board\"}\n```".to_owned()),
        ("The scenario: Pay".to_owned(), "```json\n{\"passed\": false, \"said\": \"checkout answered 500\"}\n```".to_owned()),
    ]);
    let (c, _) = d.client().await;
    let s = create(&c, &repo.to_string_lossy(), None).await;
    c.request(Request::ProofRun { session: s.id, anyway: false }).await.unwrap();
    let p = until(&c, s.id, "proved", |s| s.proof.as_ref().is_some_and(|p| p.verdict.done())).await.proof.unwrap();
    assert_eq!(p.verdict, Verdict::Failed, "{}", p.report());
    assert_eq!(p.shots.len(), 1);
    assert!(p.shots[0].ends_with("board.png") && std::path::Path::new(&p.shots[0]).starts_with(d.cfg.paths.home.join("shots")));
    let look = p.findings.iter().find(|f| f.lens == "look").expect("a look finding");
    assert_eq!((look.rule.as_deref(), look.file.as_deref(), look.severity.clone()), (Some("roomy"), Some("board.png"), Severity::Warn));
    let tried: Vec<(&str, bool, bool)> = p.tried.iter().map(|t| (t.id.as_str(), t.ok, t.holdout)).collect();
    assert_eq!(tried, vec![("smoke", true, false), ("pay", false, true), ("sign-in", true, false)], "the script runs first for free, then the testers; search isn't for a change this small");
    assert!(p.tried[1].said.contains("500"));
    let asked = d.backend.asked.lock().unwrap().clone();
    assert!(!asked.iter().any(|(q, _)| q.contains("reviewing a change")), "a failed scenario stops it before the reviewers");
    assert!(asked.iter().any(|(q, m)| q.contains("judge how it looks") && q.contains("roomy") && m == "sonnet"), "the look rule goes to a model that can see");
    let jobs = d.backend.jobs.lock().unwrap().clone();
    let tester = jobs.iter().find(|j| j.0.contains("Bash")).expect("a tester");
    assert!(tester.1, "a tester acts without asking");
    assert_eq!(tester.3, cfg!(target_os = "macos"), "in a sandbox where there is one");
    assert!(jobs.iter().any(|j| j.0 == "Read" && j.2.iter().any(|d| d.ends_with(s.id.to_string()))), "the look reviewer can read the screenshots");
    assert_eq!(std::fs::read_to_string(repo.join("a.txt")).unwrap(), "two\n");

    let Response::Shot { png, .. } = c.request(Request::Shot { path: p.shots[0].clone() }).await.unwrap() else { panic!() };
    assert_eq!(png, "cG5n");
    assert!(c.request(Request::Shot { path: "/etc/hosts".into() }).await.is_err(), "only screenshots");
}

#[tokio::test]
async fn a_checkup_runs_everything_on_main_and_files_a_task_for_each_thing_that_broke() {
    use tessel_proto::proof::Verdict;
    let d = Daemon::start(&["plain"]).await;
    let repo = d.cfg.paths.home.join("app");
    std::fs::create_dir_all(repo.join(".tessel/scenarios")).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    std::fs::write(repo.join(".tessel/settings.toml"), "[proof]\nverify = [\"true\", \"echo 'error: lint' && false\"]\n[factory]\ncheckup = \"draft\"\n").unwrap();
    std::fs::write(repo.join(".tessel/scenarios/export.md"), "---\nrun: test -f export.csv\n---\n# Export\n").unwrap();
    std::fs::write(repo.join(".tessel/scenarios/login.md"), "# Login\nLog in.\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    d.backend.reviews.lock().unwrap().push(("The scenario: Login".to_owned(), "```json\n{\"passed\": true, \"said\": \"fine\"}\n```".to_owned()));
    let (c, _) = d.client().await;
    let Response::Checkup { proof, filed } = c.request(Request::Checkup { repo: repo.to_string_lossy().into_owned() }).await.unwrap() else { panic!() };
    assert_eq!(proof.verdict, Verdict::Failed);
    assert_eq!(filed.len(), 2, "{}", proof.report());
    let Response::Tasks { tasks } = c.request(Request::TaskList).await.unwrap() else { panic!() };
    let titles: Vec<&str> = tasks.iter().map(|t| t.title.as_str()).collect();
    assert!(titles.contains(&"Fix `echo 'error: lint' && false` failing on main") && titles.contains(&"Fix: Export fails on main"), "{titles:?}");
    assert!(tasks.iter().all(|t| t.state == tessel_proto::task::TaskState::Draft), "this repository wants them as drafts");
    assert!(tasks.iter().any(|t| t.spec.contains("error: lint")));
    assert!(proof.tried.iter().any(|t| t.id == "login" && t.ok), "every scenario runs in a checkup");
    let Response::Checkup { filed: again, .. } = c.request(Request::Checkup { repo: repo.to_string_lossy().into_owned() }).await.unwrap() else { panic!() };
    assert!(again.is_empty(), "the same breakage isn't filed twice");
    assert_eq!(git(&repo, &["worktree", "list"]).lines().count(), 1, "its scratch checkout is gone");
    assert!(d.cfg.paths.home.join("checkups/app.json").is_file());
}

#[tokio::test]
async fn a_paused_factory_starts_nothing_until_it_resumes() {
    use tessel_proto::task::TaskState;
    let d = Daemon::start(&["plain+"]).await;
    let repo = d.cfg.paths.home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"], &["add", "-A"], &["commit", "-q", "-m", "feat: start"]] {
        git(&repo, args);
    }
    let (c, _) = d.client().await;
    let Response::Settings { settings } = c.request(Request::SetSettings { patch: serde_json::json!({"paused": true}) }).await.unwrap() else { panic!() };
    assert!(settings.paused);
    let make = Request::TaskCreate { repo: repo.to_string_lossy().into_owned(), title: "Count higher".into(), spec: String::new(), deps: vec![], parent: None, ready: true, budget_usd: None, plan: false, from: None, attachments: vec![] };
    let Response::Task { task } = c.request(make).await.unwrap() else { panic!() };
    tokio::time::sleep(Duration::from_millis(600)).await;
    let waiting = task_until(&c, &task.id, "waits", |t| t.state == TaskState::Ready).await;
    assert!(waiting.session.is_none(), "nothing starts while paused");
    c.request(Request::SetSettings { patch: serde_json::json!({"paused": false}) }).await.unwrap();
    let going = task_until(&c, &task.id, "starts once resumed", |t| t.session.is_some()).await;
    assert_ne!(going.state, TaskState::Ready);
}
