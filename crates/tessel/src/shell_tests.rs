//! the window, driven the way a person drives it: clicks on what is on
//! screen, real key presses through the real key bindings, typed text.
//!
//! each test stands in for the daemon. it feeds the window events and checks
//! what the window asks the daemon to do.

use crate::bridge::{Bridge, Cmd, UiEvent};
use crate::shell::{Launch, Shell};
use gpui_kit::test::TestWindowExt as _;
// named imports only: a wildcard would bring in the library's own `test`
// item, which the test attribute then expands into forever.
use crate::shell::Tab;
use gpui_kit::{AnyWindowHandle, AppContext as _, ClipboardItem, ElementId, Entity, Image, ImageFormat, ScrollDelta, ScrollEvent, SharedString, TestAppContext, TouchPhase, WindowOptions, point, px};
use tessel_proto::*;
use std::time::Duration;
use tokio::sync::mpsc;

struct Rig {
    window: AnyWindowHandle,
    shell: Entity<Shell>,
    events: mpsc::UnboundedSender<UiEvent>,
    cmds: mpsc::UnboundedReceiver<Cmd>,
}

fn sid(n: u128) -> SessionId {
    SessionId(uuid::Uuid::from_u128(n))
}

fn session(n: u128, goal: &str, state: RunState, attention: Option<AttentionReason>) -> SessionSummary {
    SessionSummary {
        id: sid(n),
        goal: goal.into(),
        // each in a workspace of its own (a worktree of one repository), as sessions are by default.
        cwd: format!("/work/api-{n}"),
        worktree: Some(tessel_proto::WorktreeInfo { repo: "/work/api".into(), branch: format!("tessel/s{n}") }),
        parent: None,
        activity: match state {
            RunState::Working => Some("running cargo test".into()),
            RunState::NeedsPermission => Some("wants to: delete the build folder".into()),
            _ => None,
        },
        activity_since_ms: Some(1),
        run_state: state,
        attention: attention.map(|reason| Attention { reason, since_ms: 1, seq: 1 }),
        warm: true,
        background_tasks: 0,
        model: None,
        permission_mode: None,
        effort: None,
        fast: false,
        context_used: None,
        context_window: None,
        tasks: vec![], mcp: vec![], linked_dirs: vec![],
        cost_usd: 0.0,
        trace: vec![],
        asking: None,
        last_seq: 5,
        created_ms: n as u64,
        updated_ms: 0,
    }
}

fn permission(id: &str) -> PermissionRequest {
    PermissionRequest {
        request_id: RequestId::from(id),
        tool_item: None,
        tool: "Bash".into(),
        input: serde_json::json!({"command": "rm -rf build", "description": "Delete the build folder"}),
        description: None,
        is_question: false,
        plan: None,
        suggestions: vec![],
    }
}

fn question(id: &str) -> PermissionRequest {
    PermissionRequest {
        request_id: RequestId::from(id),
        tool_item: None,
        tool: "AskUserQuestion".into(),
        input: serde_json::json!({"questions": [{"question": "Red or blue?", "options": [{"label": "Red"}, {"label": "Blue"}]}]}),
        description: None,
        is_question: true,
        plan: None,
        suggestions: vec![],
    }
}

fn snapshot(summary: SessionSummary, pending: Vec<PermissionRequest>) -> UiEvent {
    let item = Item { id: ItemId::from("u1"), parent: None, seq: 1, order: 1, created_ms: 0, body: ItemBody::User { text: "do the thing".into(), attachments: vec![] } };
    UiEvent::Snapshot(Box::new(SessionSnapshot { summary, items: vec![item], has_more: false, pending }))
}

impl Rig {
    fn new(cx: &mut TestAppContext) -> Self {
        Self::launched(cx, Launch::default())
    }

    fn launched(cx: &mut TestAppContext, launch: Launch) -> Self {
        let (cmd_tx, cmds) = mpsc::unbounded_channel();
        let (events, event_rx) = mpsc::unbounded_channel();
        let (window, shell) = cx.update(|cx| {
            gpui_kit::init(cx);
            crate::bind_keys(cx);
            let (window, shell) = gpui_kit::open_window(WindowOptions::default(), cx, |window, cx| {
                cx.new(|cx| Shell::new(Bridge { cmds: cmd_tx }, event_rx, launch, window, cx))
            })
            .unwrap();
            crate::bind_actions(shell.downgrade(), cx);
            (window, shell)
        });
        // a window you are looking at. (seeing a finished session only counts in one.)
        cx.update_window(window, |_, window, _| window.activate_window()).unwrap();
        let mut rig = Self { window, shell, events, cmds };
        rig.settle(cx);
        rig
    }

    /// let everything that is queued happen, and let anything moving come to rest.
    fn settle(&mut self, cx: &mut TestAppContext) {
        for _ in 0..40 {
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(50));
            self.frame(cx);
        }
        cx.run_until_parked();
    }

    fn frame(&mut self, cx: &mut TestAppContext) {
        cx.update_window(self.window, |_, window, cx| window.render_frame(cx)).unwrap();
    }

    fn feed(&mut self, cx: &mut TestAppContext, events: Vec<UiEvent>) {
        for ev in events {
            self.events.send(ev).unwrap();
        }
        self.settle(cx);
    }

    fn click(&mut self, cx: &mut TestAppContext, id: impl Into<ElementId>) {
        let id = id.into();
        cx.update_window(self.window, |_, window, cx| window.click(id, cx)).unwrap();
        self.settle(cx);
    }

    fn press(&mut self, cx: &mut TestAppContext, key: &str) {
        cx.update_window(self.window, |_, window, cx| window.press(key, cx)).unwrap();
        self.settle(cx);
    }

    fn typed(&mut self, cx: &mut TestAppContext, text: &str) {
        cx.update_window(self.window, |_, window, cx| window.input(text, cx)).unwrap();
        self.settle(cx);
    }

    fn exists(&mut self, cx: &mut TestAppContext, id: impl Into<ElementId>) -> bool {
        let id = id.into();
        cx.update_window(self.window, |_, window, _| window.try_find(id).is_some()).unwrap()
    }

    /// everything the window has asked the daemon to do since last asked.
    fn asked(&mut self) -> Vec<Cmd> {
        let mut out = Vec::new();
        while let Ok(c) = self.cmds.try_recv() {
            // asking whether a folder is a repository is the form's own plumbing.
            if !matches!(c, Cmd::Ask { tag: "folder.info", .. } | Cmd::Ask { req: tessel_proto::Request::Scripts { .. }, .. }) {
                out.push(c);
            }
        }
        out
    }

    /// draw the window from this layout from now on.
    fn layout(&mut self, cx: &mut TestAppContext, json: &str) {
        let shell = self.shell.clone();
        cx.update_window(self.window, |_, window, cx| shell.update(cx, |s, cx| s.use_ui_text(json, window, cx))).unwrap().expect("the layout parses");
        self.settle(cx);
    }

    /// a window showing three sessions with `open` on screen.
    fn with_sessions(cx: &mut TestAppContext, open: SessionSummary, pending: Vec<PermissionRequest>) -> Self {
        Self::with_sessions_launched(cx, open, pending, Launch::default())
    }

    fn with_sessions_launched(cx: &mut TestAppContext, open: SessionSummary, pending: Vec<PermissionRequest>, launch: Launch) -> Self {
        let mut rig = Self::launched(cx, launch);
        let sessions = vec![
            open.clone(),
            session(2, "Split the auth module", RunState::Idle, None),
            session(3, "Write the docs", RunState::Idle, Some(AttentionReason::Finished)),
        ];
        rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(sessions)]);
        // connecting asks which models there are; that is not what these tests are about.
        rig.asked();
        rig.click(cx, open.id.0);
        assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == open.id), "clicking a row opens it");
        rig.feed(cx, vec![snapshot(open, pending)]);
        rig.asked();
        rig
    }
}

#[gpui_kit::test]
fn clicking_a_session_opens_it(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Sessions(vec![session(1, "Fix login", RunState::Idle, None), session(2, "Split auth", RunState::Idle, None)])]);
    rig.click(cx, sid(2).0);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == sid(2)));
    // clicking the one that is already open asks for nothing.
    rig.feed(cx, vec![snapshot(session(2, "Split auth", RunState::Idle, None), vec![])]);
    rig.click(cx, sid(2).0);
    assert!(rig.asked().is_empty());
}

#[gpui_kit::test]
fn a_session_can_be_answered_without_opening_it(cx: &mut TestAppContext) {
    // the notch answers whichever session is asking, not the open one.
    let open = session(1, "Clean up", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, open, vec![]);
    rig.asked();
    rig.shell.update(cx, |s, cx| s.answer_in(sid(7), tessel_proto::RequestId::from("r"), false, cx));
    match rig.asked().as_slice() {
        [Cmd::Answer { session, request_id, decision: PermissionDecision::Deny { .. } }] => assert_eq!((*session, request_id.0.as_str()), (sid(7), "r")),
        other => panic!("{other:?}"),
    }
}

#[gpui_kit::test]
fn allow_and_deny_send_the_right_answer_for_the_right_request(cx: &mut TestAppContext) {
    let blocked = session(1, "Clean up", RunState::NeedsPermission, Some(AttentionReason::NeedsPermission));
    let mut rig = Rig::with_sessions(cx, blocked.clone(), vec![permission("first"), permission("second")]);

    rig.click(cx, ("allow", 1usize));
    match rig.asked().as_slice() {
        [Cmd::Answer { session, request_id, decision: PermissionDecision::Allow { updated_input: None } }] => {
            assert_eq!((*session, request_id.0.as_str()), (blocked.id, "second"), "the second card's button answers the second request");
        }
        other => panic!("{other:?}"),
    }
    rig.click(cx, ("deny", 0usize));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Answer { request_id, decision: PermissionDecision::Deny { .. }, .. }] if request_id.0 == "first"));
}

#[gpui_kit::test]
fn command_return_allows_the_oldest_request_and_never_answers_a_question(cx: &mut TestAppContext) {
    let blocked = session(1, "Clean up", RunState::NeedsPermission, Some(AttentionReason::NeedsPermission));
    let mut rig = Rig::with_sessions(cx, blocked, vec![question("q"), permission("p1"), permission("p2")]);
    rig.press(cx, "cmd-enter");
    assert!(
        matches!(rig.asked().as_slice(), [Cmd::Answer { request_id, decision: PermissionDecision::Allow { .. }, .. }] if request_id.0 == "p1"),
        "the question is skipped: a shortcut must not invent an answer"
    );

    // with only a question pending, the shortcut does nothing at all.
    let asking = session(1, "Clean up", RunState::NeedsInput, Some(AttentionReason::NeedsInput));
    rig.feed(cx, vec![UiEvent::Pending { session: asking.id, pending: vec![question("q")] }]);
    rig.press(cx, "cmd-enter");
    assert!(rig.asked().is_empty());
}

#[gpui_kit::test]
fn picking_an_option_answers_the_question_with_that_option(cx: &mut TestAppContext) {
    let asking = session(1, "Choose", RunState::NeedsInput, Some(AttentionReason::NeedsInput));
    let mut rig = Rig::with_sessions(cx, asking, vec![question("q")]);
    // the second option of the first question of the first card.
    rig.click(cx, ("option", 1usize));
    match rig.asked().as_slice() {
        [Cmd::Answer { request_id, decision: PermissionDecision::Allow { updated_input: Some(input) }, .. }] => {
            assert_eq!(request_id.0, "q");
            assert_eq!(input["answers"]["Red or blue?"], "Blue");
            assert_eq!(input["questions"][0]["options"].as_array().unwrap().len(), 2, "the original question is sent back with the answer");
        }
        other => panic!("{other:?}"),
    }
}

#[gpui_kit::test]
fn typing_and_return_sends_the_message_and_clears_the_field(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.click(cx, "composer");
    rig.typed(cx, "now run the tests");
    assert!(rig.asked().is_empty(), "typing alone sends nothing");
    rig.press(cx, "enter");
    match rig.asked().as_slice() {
        [Cmd::Send { session, text, .. }] => assert_eq!((*session, text.as_str()), (idle.id, "now run the tests")),
        other => panic!("{other:?}"),
    }
    // the field is empty again: sending twice does not resend.
    rig.press(cx, "enter");
    assert!(rig.asked().is_empty(), "an empty message is not sent");

    // the send button does the same as return.
    rig.typed(cx, "and commit");
    rig.click(cx, "send");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Send { text, .. }] if text == "and commit"));
}

#[gpui_kit::test]
fn command_period_stops_a_busy_agent_and_only_a_busy_one(cx: &mut TestAppContext) {
    let working = session(1, "Run tests", RunState::Working, None);
    let mut rig = Rig::with_sessions(cx, working.clone(), vec![]);
    assert!(rig.exists(cx, "toolbar-stop"), "a busy session shows Stop");
    rig.press(cx, "cmd-.");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Interrupt(id)] if *id == working.id));
    rig.click(cx, "toolbar-stop");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Interrupt(_)]));

    rig.feed(cx, vec![UiEvent::Session(session(1, "Run tests", RunState::Idle, None))]);
    assert!(!rig.exists(cx, "toolbar-stop"), "an idle session has nothing to stop");
    rig.press(cx, "cmd-.");
    assert!(rig.asked().iter().all(|c| !matches!(c, Cmd::Interrupt(_))));
}

#[gpui_kit::test]
fn looking_at_a_finished_session_marks_it_seen(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    let done = session(3, "Write the docs", RunState::Idle, Some(AttentionReason::Finished));
    rig.feed(cx, vec![UiEvent::Sessions(vec![done.clone(), session(2, "Other", RunState::Idle, None)])]);
    rig.click(cx, done.id.0);
    rig.feed(cx, vec![snapshot(done.clone(), vec![])]);
    let asked = rig.asked();
    assert!(
        asked.iter().any(|c| matches!(c, Cmd::MarkSeen { session, seq } if *session == done.id && *seq == done.last_seq)),
        "opening it is looking at it: {asked:?}"
    );
}

#[gpui_kit::test]
fn command_j_goes_to_whoever_has_waited_longest(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    let mut old = session(1, "Old question", RunState::NeedsInput, Some(AttentionReason::NeedsInput));
    old.attention.as_mut().unwrap().since_ms = 10;
    let mut newer = session(2, "Newer prompt", RunState::NeedsPermission, Some(AttentionReason::NeedsPermission));
    newer.attention.as_mut().unwrap().since_ms = 500;
    rig.feed(cx, vec![UiEvent::Sessions(vec![newer, old, session(3, "Quiet", RunState::Idle, None)])]);
    rig.press(cx, "cmd-j");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == sid(1)), "the one that has waited longest first");
}

#[gpui_kit::test]
fn deleting_takes_two_presses(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.click(cx, "tab-info");
    assert!(rig.exists(cx, "delete-session"), "the panel is open on the session's own page");
    rig.asked();
    rig.click(cx, "delete-session");
    assert!(rig.asked().iter().all(|c| !matches!(c, Cmd::Delete { .. })), "one press only arms it");
    rig.click(cx, "delete-session");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Delete { session, remove_worktree: false }] if *session == idle.id));
}

#[gpui_kit::test]
fn opening_another_session_disarms_a_half_pressed_delete(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.click(cx, "tab-info");
    rig.click(cx, "delete-session");
    rig.click(cx, sid(2).0);
    rig.feed(cx, vec![snapshot(session(2, "Split the auth module", RunState::Idle, None), vec![])]);
    rig.asked();
    rig.click(cx, "delete-session");
    assert!(rig.asked().iter().all(|c| !matches!(c, Cmd::Delete { .. })), "the arming belonged to the other session");
}

#[gpui_kit::test]
fn the_sidebar_and_inspector_open_and_close_from_keys_and_toolbar(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    assert!(rig.exists(cx, sid(2).0), "sidebar rows are on screen");
    rig.press(cx, "alt-cmd-s");
    assert!(!rig.exists(cx, sid(2).0), "the sidebar is gone once it has finished closing");
    rig.click(cx, "toolbar-sidebar");
    assert!(rig.exists(cx, sid(2).0), "and back");

    assert!(!rig.exists(cx, "tab-changes"));
    rig.click(cx, "toolbar-inspector");
    assert!(rig.exists(cx, "tab-changes"));
    rig.press(cx, "alt-cmd-i");
    assert!(!rig.exists(cx, "tab-changes"));

    // focus hides both; again brings the sidebar back.
    rig.click(cx, "toolbar-inspector");
    rig.press(cx, "shift-cmd-f");
    assert!(!rig.exists(cx, sid(2).0) && !rig.exists(cx, "delete-session"));
    rig.press(cx, "shift-cmd-f");
    assert!(rig.exists(cx, sid(2).0));
}

#[gpui_kit::test]
fn a_new_session_is_created_from_the_form(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![])]);
    rig.asked();
    rig.click(cx, "new-prompt");
    rig.typed(cx, "fix the flaky test");
    rig.click(cx, "create");
    assert!(rig.asked().is_empty(), "with no project there's nowhere to start: nothing is guessed, and what was typed stays");

    // the projects are the sidebar's, the latest first; one is picked.
    let mut api = session(1, "Fix login", RunState::Idle, None);
    api.updated_ms = 2;
    let mut web = session(2, "Polish", RunState::Idle, None);
    web.cwd = "/work/web".into();
    web.worktree = None;
    web.updated_ms = 1;
    rig.feed(cx, vec![UiEvent::Sessions(vec![api, web])]);
    rig.press(cx, "cmd-n");
    rig.click(cx, "new-project");
    assert!(rig.exists(cx, "new-project-0") && rig.exists(cx, "new-project-1") && !rig.exists(cx, "new-project-2"));
    assert!(rig.exists(cx, "new-project-open") && rig.exists(cx, "new-project-github") && rig.exists(cx, "new-project-create"), "and the ways to another, as the + menu has them");
    rig.click(cx, "new-project-1");
    rig.click(cx, "worktree-toggle");
    rig.click(cx, "create");
    match rig.asked().as_slice() {
        [Cmd::Create { cwd, worktree: true, prompt }] => assert_eq!((cwd.as_str(), prompt.as_str()), ("/work/web", "fix the flaky test")),
        other => panic!("{other:?}"),
    }
    // command-n brings the form back from a session.
    let idle = session(1, "Fix login", RunState::Idle, None);
    rig.feed(cx, vec![UiEvent::Sessions(vec![idle.clone()]), UiEvent::Created, snapshot(idle, vec![])]);
    assert!(!rig.exists(cx, "create"));
    rig.press(cx, "cmd-n");
    assert!(rig.exists(cx, "create"));
}

#[gpui_kit::test]
fn rows_hold_their_places_under_the_pointer_and_settle_when_it_leaves(cx: &mut TestAppContext) {
    use crate::model::Entry;
    let mut rig = Rig::new(cx);
    let quiet = |n, goal| session(n, goal, RunState::Idle, None);
    rig.feed(cx, vec![UiEvent::Sessions(vec![quiet(1, "One"), quiet(2, "Two"), quiet(3, "Three")])]);
    let order = |rig: &Rig, cx: &mut TestAppContext| -> Vec<u128> {
        rig.shell.read_with(cx, |s, _| s.shown_for_test().entries.iter().filter_map(|e| match e { Entry::Row(id) => Some(id.0.as_u128()), _ => None }).collect())
    };
    assert_eq!(order(&rig, cx), [3, 2, 1]);

    // the pointer comes to rest on a row; then session 1 starts needing you.
    cx.update_window(rig.window, |_, window, cx| window.hover(sid(2).0, cx)).unwrap();
    rig.settle(cx);
    rig.feed(cx, vec![UiEvent::Session(session(1, "One", RunState::NeedsPermission, Some(AttentionReason::NeedsPermission)))]);
    assert_eq!(order(&rig, cx), [3, 2, 1], "nothing moved under the pointer");
    // the click lands on the row that was there when the pointer arrived.
    rig.click(cx, sid(2).0);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == sid(2)));

    // the pointer leaves the list: now the queue forms.
    cx.update_window(rig.window, |_, window, cx| window.hover("toolbar-overview", cx)).unwrap();
    rig.settle(cx);
    assert_eq!(order(&rig, cx), [1, 3, 2], "the session that needs you is on top");
}

#[gpui_kit::test]
fn folding_a_project_happens_at_once_with_the_pointer_on_the_list(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    let quiet = |n, goal| session(n, goal, RunState::Idle, None);
    rig.feed(cx, vec![UiEvent::Sessions(vec![quiet(1, "One"), quiet(2, "Two")])]);
    let rows = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.shown_for_test().rows().count());
    assert_eq!(rows(&rig, cx), 2);
    // the pointer is on the header it clicks: the rows still go, and come back, straight away.
    cx.update_window(rig.window, |_, window, cx| window.hover("project-api", cx)).unwrap();
    rig.settle(cx);
    rig.click(cx, "project-api");
    assert_eq!(rows(&rig, cx), 0, "folded at once");
    assert!(rig.exists(cx, "project-api"), "and its header stays to unfold it");
    rig.click(cx, "project-api");
    assert_eq!(rows(&rig, cx), 2, "unfolded at once");
}

// ---- the overview -------------------------------------------------------------

fn card(n: u128) -> ElementId {
    ElementId::from(gpui_kit::SharedString::from(format!("card-{}", sid(n))))
}

#[gpui_kit::test]
fn the_overview_shows_every_session_and_picking_a_card_opens_it(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    assert!(!rig.exists(cx, card(2)));

    rig.press(cx, "shift-cmd-\\");
    for n in [1, 2, 3] {
        assert!(rig.exists(cx, card(n)), "session {n} has a card");
    }
    rig.click(cx, card(2));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == sid(2)));
    assert!(!rig.exists(cx, card(2)), "the overview has gone once it finished closing");
    assert!(rig.exists(cx, sid(2).0), "and the sidebar row is back in reach");
}

#[gpui_kit::test]
fn escape_leaves_the_overview_without_changing_what_is_open(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.click(cx, "toolbar-overview");
    assert!(rig.exists(cx, card(1)));
    rig.press(cx, "escape");
    assert!(!rig.exists(cx, card(1)));
    assert!(rig.asked().is_empty(), "nothing was opened, nothing was asked of the daemon");
    // the toolbar button toggles: once more opens it, once more closes it.
    rig.click(cx, "toolbar-overview");
    assert!(rig.exists(cx, card(1)));
    rig.press(cx, "shift-cmd-\\");
    assert!(!rig.exists(cx, card(1)));
}

#[gpui_kit::test]
fn arrows_and_return_pick_a_card_from_the_keyboard(cx: &mut TestAppContext) {
    use crate::model::Entry;
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    // cards are in the sidebar's order.
    let order: Vec<SessionId> = rig.shell.read_with(cx, |s, _| s.shown_for_test().entries.iter().filter_map(|e| match e { Entry::Row(id) => Some(*id), _ => None }).collect());
    assert_eq!(order.len(), 3);
    let open_at = order.iter().position(|id| *id == sid(1)).unwrap();

    rig.press(cx, "shift-cmd-\\");
    // the cursor starts on the session that was open; return on it changes nothing but closes.
    rig.press(cx, "enter");
    assert!(rig.asked().is_empty(), "it was already open");
    assert!(!rig.exists(cx, card(1)));

    rig.press(cx, "shift-cmd-\\");
    // move to a different card. left or right, whichever exists from here.
    let (key, target) = if open_at + 1 < order.len() { ("right", order[open_at + 1]) } else { ("left", order[open_at - 1]) };
    rig.press(cx, key);
    rig.press(cx, "enter");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == target), "return opens the card the arrows landed on");
}

#[gpui_kit::test]
fn arrow_keys_in_the_message_field_do_not_drive_the_overview(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.click(cx, "composer");
    rig.typed(cx, "hello");
    rig.press(cx, "left");
    rig.press(cx, "escape");
    rig.press(cx, "enter");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Send { text, .. }] if text == "hello"), "with the overview closed, these keys belong to the field");
}

// ---- the divider ----------------------------------------------------------------

#[gpui_kit::test]
fn dragging_the_divider_resizes_the_sidebar_and_throwing_it_left_closes_it(cx: &mut TestAppContext) {
    use gpui_kit::{point, px};
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    let width = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.sidebar_width_for_test());
    let grip = |rig: &Rig, cx: &mut TestAppContext| {
        let b = cx.update_window(rig.window, |_, window, _| window.find("sidebar-divider-grip").bounds()).unwrap();
        point(b.origin.x + b.size.width / 2.0, b.origin.y + b.size.height / 2.0)
    };
    let start = width(&rig, cx);
    assert_eq!(start, crate::theme::SIDEBAR_W);

    // drag it 50 points to the right: the sidebar is 50 points wider and stays there.
    let from = grip(&rig, cx);
    cx.update_window(rig.window, |_, window, cx| window.drag(from, point(from.x + px(50.), from.y), cx)).unwrap();
    rig.settle(cx);
    assert!((width(&rig, cx) - (start + 50.0)).abs() < 2.0, "sidebar is {} wide", width(&rig, cx));
    assert!(rig.exists(cx, sid(2).0));

    // it will not grow past its limit however far you pull.
    let from = grip(&rig, cx);
    cx.update_window(rig.window, |_, window, cx| window.drag(from, point(from.x + px(600.), from.y), cx)).unwrap();
    rig.settle(cx);
    assert_eq!(width(&rig, cx), crate::theme::SIDEBAR_MAX);

    // dragged most of the way shut, it closes the rest of the way itself.
    let from = grip(&rig, cx);
    cx.update_window(rig.window, |_, window, cx| window.drag(from, point(px(40.), from.y), cx)).unwrap();
    rig.settle(cx);
    assert_eq!(width(&rig, cx), 0.0);
    assert!(!rig.exists(cx, sid(2).0), "the sidebar is closed");

    // and the toolbar button brings it back at a usable width, not the sliver it was dragged to.
    rig.click(cx, "toolbar-sidebar");
    assert!(width(&rig, cx) >= crate::theme::SIDEBAR_MIN);
    assert!(rig.exists(cx, sid(2).0));
}

// ---- the workspace panel ----------------------------------------------------

fn eid(text: impl Into<String>) -> ElementId {
    ElementId::from(SharedString::from(text.into()))
}

fn change(path: &str, status: ChangeStatus, added: u32, removed: u32) -> FileChange {
    FileChange { path: path.into(), status, added, removed, binary: false }
}

fn git(files: Vec<FileChange>) -> GitSummary {
    let (added, removed) = (files.iter().map(|f| f.added).sum(), files.iter().map(|f| f.removed).sum());
    GitSummary { is_repo: true, branch: Some("tessel/fix-login".into()), base: Some("origin/main".into()), ahead: 2, files, added, removed }
}

const PATCH: &str = "diff --git a/src/login.rs b/src/login.rs\n--- a/src/login.rs\n+++ b/src/login.rs\n@@ -1,3 +1,3 @@\n fn login() {\n-    old();\n+    new();\n }\n";

fn tool(id: &str, order: u64, name: &str, input: serde_json::Value, status: ToolStatus) -> Item {
    Item {
        id: ItemId::from(id),
        parent: None,
        seq: order,
        order,
        created_ms: 0,
        body: ItemBody::Tool { tool: name.into(), input, status, output: None, started_ms: Some(1), finished_ms: Some(2) },
    }
}

fn snapshot_of(summary: SessionSummary, items: Vec<Item>, pending: Vec<PermissionRequest>) -> UiEvent {
    UiEvent::Snapshot(Box::new(SessionSnapshot { summary, items, has_more: false, pending }))
}

#[gpui_kit::test]
fn a_change_that_disappears_is_never_asked_for(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.asked();
    let both = vec![change("build/cache.pyc", ChangeStatus::Untracked, 0, 0), change("src/login.rs", ChangeStatus::Modified, 1, 1)];
    rig.feed(cx, vec![UiEvent::Git { session: idle.id, summary: git(both) }]);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Diff { path, .. }] if path == "build/cache.pyc"));

    // the agent touches the folder; meanwhile the picked file is deleted.
    rig.shell.update(cx, |s, _| {
        s.workspace_touched();
        s.workspace_tick();
    });
    assert!(matches!(rig.asked().as_slice(), [Cmd::Git(_)]), "the status first, not a diff of a file that may be gone");
    rig.feed(cx, vec![UiEvent::Git { session: idle.id, summary: git(vec![change("src/login.rs", ChangeStatus::Modified, 1, 1)]) }]);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Diff { path, .. }] if path == "src/login.rs"), "it moves to what's still there");
}

#[gpui_kit::test]
fn the_panel_shows_what_changed_and_the_diff_of_the_file_you_pick(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    assert!(rig.asked().is_empty(), "a closed panel asks for nothing");
    rig.press(cx, "alt-cmd-i");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Git(id)] if *id == idle.id), "opening it looks at the folder");

    let files = vec![change("src/login.rs", ChangeStatus::Modified, 1, 1), change("notes.md", ChangeStatus::Untracked, 4, 0)];
    rig.feed(cx, vec![UiEvent::Git { session: idle.id, summary: git(files) }]);
    assert!(rig.exists(cx, eid("change-src/login.rs")) && rig.exists(cx, eid("change-notes.md")));
    // the first change is shown without being asked for.
    assert!(matches!(rig.asked().as_slice(), [Cmd::Diff { path, .. }] if path == "src/login.rs"));
    let diff = FileDiff { path: "src/login.rs".into(), old: None, new: None, unified: PATCH.into(), binary: false, truncated: false };
    rig.feed(cx, vec![UiEvent::Diff { session: idle.id, diff }]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.diff.is_some() && s.ws.diff_note.is_none()), "the patch was understood");
    assert!(rig.exists(cx, "diff-view"), "and is on screen");

    rig.click(cx, eid("change-notes.md"));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Diff { path, .. }] if path == "notes.md"));
    // an answer for a file you have since left is not shown over the one you chose.
    let late = FileDiff { path: "src/login.rs".into(), old: None, new: None, unified: PATCH.into(), binary: false, truncated: false };
    rig.feed(cx, vec![UiEvent::Diff { session: idle.id, diff: late }]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.diff.is_none()));
    let binary = FileDiff { path: "notes.md".into(), old: None, new: None, unified: String::new(), binary: true, truncated: false };
    rig.feed(cx, vec![UiEvent::Diff { session: idle.id, diff: binary }]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.diff_note.as_deref() == Some("This is a binary file.")));

    rig.click(cx, "diff-mode");
    assert!(rig.shell.read_with(cx, |s, _| s.ws.split), "side by side");
}

#[gpui_kit::test]
fn the_changes_refresh_when_the_agent_finishes_a_tool_but_only_while_the_panel_is_open(cx: &mut TestAppContext) {
    let working = session(1, "Fix login", RunState::Working, None);
    let mut rig = Rig::with_sessions(cx, working.clone(), vec![]);
    let edit = tool("t1", 2, "Edit", serde_json::json!({"file_path": "/work/api/src/login.rs"}), ToolStatus::Ok);
    rig.feed(cx, vec![UiEvent::Item { session: working.id, item: edit.clone() }]);
    assert!(rig.asked().is_empty(), "nobody is looking at the changes");
    rig.press(cx, "alt-cmd-i");
    rig.asked();
    rig.feed(cx, vec![UiEvent::Item { session: working.id, item: Item { seq: 3, ..edit } }]);
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Git(id) if *id == working.id)), "the folder changed, so look again");
}

#[gpui_kit::test]
fn files_can_be_browsed_edited_and_saved_and_unsaved_edits_are_never_dropped(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.asked();
    rig.click(cx, "tab-files");
    assert!(matches!(rig.asked().as_slice(), [Cmd::List { path, .. }] if path.is_empty()));
    let dir = |name: &str, is_dir: bool| DirEntry { name: name.into(), is_dir, size: 10 };
    rig.feed(cx, vec![UiEvent::Dir { session: idle.id, path: String::new(), entries: vec![dir("src", true), dir("README.md", false)] }]);
    assert!(rig.exists(cx, eid("file-src")) && rig.exists(cx, eid("file-README.md")));

    rig.click(cx, eid("file-src"));
    assert!(matches!(rig.asked().as_slice(), [Cmd::List { path, .. }] if path == "src"), "a folder is listed the first time it is opened");
    rig.feed(cx, vec![UiEvent::Dir { session: idle.id, path: "src".into(), entries: vec![dir("main.rs", false)] }]);
    rig.click(cx, eid("file-src/main.rs"));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Read { path, .. }] if path == "src/main.rs"));
    let file = FileContent { path: "src/main.rs".into(), text: Some("fn main() {}\n".into()), binary: false, truncated: false, size: 13 };
    rig.feed(cx, vec![UiEvent::File { session: idle.id, file }]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().is_some_and(|f| f.editor.is_some() && f.note.is_none())), "the file has an editor");
    assert!(
        rig.asked().iter().any(|c| matches!(c, Cmd::Ask { tag: "file.marks", req: Request::GitDiff { path, .. } } if path == "src/main.rs")),
        "and asks what changed in it, for the gutter"
    );
    assert!(rig.exists(cx, "file-editor"), "editor on screen");
    assert!(rig.exists(cx, "file-close"), "close on screen");
    assert!(rig.exists(cx, "file-save"), "save on screen");

    // type into the editor.
    let editor = rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().unwrap().editor.clone().unwrap());
    cx.update_window(rig.window, |_, window, cx| editor.update(cx, |e, cx| e.focus(window, cx))).unwrap();
    rig.typed(cx, "// hi\n");
    assert!(rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().unwrap().dirty), "typing makes it unsaved");

    // another file can't replace it while there are edits to lose.
    rig.click(cx, eid("file-README.md"));
    assert!(rig.asked().is_empty());
    assert!(rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().unwrap().path == "src/main.rs"));

    rig.click(cx, "file-save");
    match rig.asked().as_slice() {
        [Cmd::Write { session, path, content }] => {
            assert_eq!((*session, path.as_str()), (idle.id, "src/main.rs"));
            assert!(content.contains("// hi") && content.contains("fn main() {}"), "{content:?}");
        }
        other => panic!("{other:?}"),
    }
    rig.feed(cx, vec![UiEvent::Saved { session: idle.id, path: "src/main.rs".into() }]);
    assert!(rig.shell.read_with(cx, |s, _| !s.ws.file.as_ref().unwrap().dirty));
    rig.asked();
    rig.click(cx, eid("file-README.md"));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Read { path, .. }] if path == "README.md"), "saved, so now it may be replaced");
}

#[gpui_kit::test]
fn each_session_keeps_its_own_open_file_and_edits(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.click(cx, "tab-files");
    let file = FileContent { path: "a.txt".into(), text: Some("one".into()), binary: false, truncated: false, size: 3 };
    rig.feed(cx, vec![UiEvent::Dir { session: idle.id, path: String::new(), entries: vec![] }, UiEvent::File { session: idle.id, file }]);
    let editor = rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().unwrap().editor.clone().unwrap());
    cx.update_window(rig.window, |_, window, cx| editor.update(cx, |e, cx| e.focus(window, cx))).unwrap();
    rig.typed(cx, "zero ");

    rig.click(cx, sid(2).0);
    rig.feed(cx, vec![snapshot(session(2, "Split the auth module", RunState::Idle, None), vec![])]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.file.is_none() && s.ws.tab == Tab::Files), "the other session has no file open, on the same tab");
    rig.click(cx, sid(1).0);
    rig.feed(cx, vec![snapshot(idle, vec![])]);
    let text = rig.shell.read_with(cx, |s, cx| s.ws.file.as_ref().map(|f| (f.dirty, f.editor.as_ref().unwrap().read(cx).value().to_string())));
    assert_eq!(text, Some((true, "zero one".to_owned())), "back where it was, edits and all");
}

#[gpui_kit::test]
fn checks_show_the_pull_request_and_refresh_on_request(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.asked();
    rig.click(cx, "tab-checks");
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::Pr(id) if *id == idle.id)) && asked.iter().any(|c| matches!(c, Cmd::Git(_))));
    assert!(!rig.exists(cx, "pr-link"));

    let check = |name: &str, state: CheckState| Check { name: name.into(), state, url: Some("https://example.com/run".into()) };
    let pr = PrSummary {
        number: 12, title: "Fix the flaky login test".into(), url: "https://example.com/pr/12".into(), state: "OPEN".into(), draft: true,
        base: "main".into(), head: "tessel/fix-login".into(), checks: vec![check("build", CheckState::Passing), check("test", CheckState::Failing)],
    };
    rig.feed(cx, vec![UiEvent::Git { session: idle.id, summary: git(vec![]) }, UiEvent::Pr { session: idle.id, status: PrStatus { pr: Some(pr), note: None } }]);
    assert!(rig.exists(cx, "pr-link") && rig.exists(cx, ("check", 0usize)) && rig.exists(cx, ("check", 1usize)));
    rig.click(cx, "checks-refresh");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Pr(_))));

    // no pull request is said plainly, with the reason the daemon gave.
    rig.feed(cx, vec![UiEvent::Pr { session: idle.id, status: PrStatus { pr: None, note: Some("No pull request for this branch yet.".into()) } }]);
    assert!(!rig.exists(cx, "pr-link"));
}

#[gpui_kit::test]
fn plans_and_files_the_agent_made_collect_as_artifacts_and_open_from_there(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let items = vec![
        tool("p1", 1, "ExitPlanMode", serde_json::json!({"plan": "1. Read the test\n2. Fix it"}), ToolStatus::Ok),
        tool("w0", 2, "Write", serde_json::json!({"file_path": "/Users/x/.claude/plans/the-plan.md"}), ToolStatus::Ok),
        tool("w1", 3, "Write", serde_json::json!({"file_path": format!("{}/docs/notes.md", idle.cwd)}), ToolStatus::Ok),
        tool("w2", 4, "Write", serde_json::json!({"file_path": format!("{}/never.md", idle.cwd)}), ToolStatus::Failed),
    ];
    rig.feed(cx, vec![snapshot_of(idle.clone(), items, vec![])]);
    rig.press(cx, "alt-cmd-i");
    rig.click(cx, "tab-artifacts");
    assert!(rig.exists(cx, eid("artifact-docs/notes.md")), "a file it created, by its path in the session's folder");
    assert!(!rig.exists(cx, eid("artifact-never.md")), "a write that failed made nothing");
    assert!(!rig.exists(cx, eid("artifact-/Users/x/.claude/plans/the-plan.md")), "the agent's own copy of the plan is not a deliverable");
    rig.asked();
    rig.click(cx, eid("artifact-docs/notes.md"));
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Read { path, .. } if path == "docs/notes.md")));
    assert!(rig.shell.read_with(cx, |s, _| s.ws.tab == Tab::Files), "and it opens in the editor");
}

#[gpui_kit::test]
fn a_linked_folder_can_be_removed_from_the_sessions_page(cx: &mut TestAppContext) {
    let mut linked = session(1, "Fix login", RunState::Idle, None);
    linked.linked_dirs = vec!["/work/shared".into()];
    linked.mcp = vec![McpServer { name: "docs".into(), status: "needs-auth".into(), scope: None }];
    let mut rig = Rig::with_sessions(cx, linked.clone(), vec![]);
    rig.press(cx, "alt-cmd-i");
    rig.click(cx, "tab-info");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::RefreshMcp(id) if *id == linked.id)), "looking at the servers asks how they are now");
    assert!(rig.exists(cx, "link-folder"));
    rig.click(cx, ("unlink", 0usize));
    assert!(matches!(rig.asked().as_slice(), [Cmd::UnlinkDir { session, path }] if *session == linked.id && path == "/work/shared"));
}

// ---- the message box ----------------------------------------------------------

#[gpui_kit::test]
fn a_plan_is_approved_or_sent_back_with_what_you_typed(cx: &mut TestAppContext) {
    let mut plan = permission("plan-1");
    plan.tool = "ExitPlanMode".into();
    plan.plan = Some("1. Read the test\n2. Fix it".into());
    let blocked = session(1, "Fix login", RunState::NeedsPermission, Some(AttentionReason::NeedsPermission));
    let mut rig = Rig::with_sessions(cx, blocked.clone(), vec![plan.clone()]);
    assert!(rig.exists(cx, ("plan-approve", 0usize)) && !rig.exists(cx, ("allow", 0usize)), "a plan is not offered as allow or deny");
    rig.click(cx, "composer");
    rig.typed(cx, "skip step one");
    rig.click(cx, ("plan-back", 0usize));
    match rig.asked().as_slice() {
        [Cmd::Answer { request_id, decision: PermissionDecision::Deny { message }, .. }] => {
            assert_eq!((request_id.0.as_str(), message.as_str()), ("plan-1", "skip step one"));
        }
        other => panic!("{other:?}"),
    }

    let mut rig = Rig::with_sessions(cx, blocked, vec![plan]);
    rig.click(cx, ("plan-approve", 0usize));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Answer { decision: PermissionDecision::Allow { .. }, .. }]));
}

#[gpui_kit::test]
fn the_model_and_mode_are_chosen_from_the_message_box(cx: &mut TestAppContext) {
    let mut idle = session(1, "Fix login", RunState::Idle, None);
    idle.model = Some("claude-sonnet-5-5".into());
    idle.permission_mode = Some("default".into());
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected]);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Models]), "the models on offer are asked for, not assumed");
    let model = |value: &str, id: &str, name: &str| ModelInfo { value: value.into(), id: id.into(), name: name.into(), description: String::new(), ..ModelInfo::default() };
    let models = vec![model("default", "claude-opus-5-5", "Default (recommended)"), model("opus", "claude-opus-5-5", "Opus 5.5"), model("sonnet", "claude-sonnet-5-5", "Sonnet 5.5")];
    rig.feed(cx, vec![UiEvent::Models(models, vec![]), UiEvent::Sessions(vec![idle.clone()])]);
    rig.click(cx, idle.id.0);
    rig.feed(cx, vec![snapshot(idle.clone(), vec![])]);
    rig.asked();
    assert!(rig.exists(cx, "model-menu") && rig.exists(cx, "mode-menu"));
    let names = rig.shell.read_with(cx, |s, _| (s.model_name("claude-sonnet-5-5"), s.model_name("claude-opus-5-5"), s.model_name("something-new")));
    assert_eq!(names, ("Sonnet 5.5".to_owned(), "Opus 5.5".to_owned(), "something-new".to_owned()), "a session's model reads as the agent names it");

    rig.shell.update(cx, |s, cx| s.set_model("opus".into(), cx));
    rig.shell.update(cx, |s, cx| s.set_mode("plan".into(), cx));
    match rig.asked().as_slice() {
        [Cmd::SetModel { session, model }, Cmd::SetMode { mode, .. }] => assert_eq!((*session, model.as_str(), mode.as_str()), (idle.id, "opus", "plan")),
        other => panic!("{other:?}"),
    }
}

#[gpui_kit::test]
fn files_go_with_the_next_message_and_can_be_taken_off_again(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let dir = std::env::temp_dir().join(format!("tessel-ui-attach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (shot, source) = (dir.join("shot.png"), dir.join("main.rs"));
    std::fs::write(&shot, [1u8, 2, 3]).unwrap();
    std::fs::write(&source, "fn main() {}").unwrap();

    let paths = vec![shot.clone(), source.clone()];
    cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.attach_paths(paths, window, cx))).unwrap();
    rig.settle(cx);
    assert!(rig.exists(cx, ("detach", 0usize)) && !rig.exists(cx, ("detach", 1usize)), "the image is attached; the source file is not");
    let typed = rig.shell.read_with(cx, |s, cx| s.composer_text(cx));
    assert!(typed.contains("main.rs"), "a file the agent can't be handed is named for it to read: {typed:?}");

    // something pasted joins it.
    let pasted = ClipboardItem::new_image(&Image::from_bytes(ImageFormat::Png, vec![9, 9]));
    let took = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.paste(&pasted, window, cx))).unwrap();
    let plain = ClipboardItem::new_string("just words".into());
    let left = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.paste(&plain, window, cx))).unwrap();
    assert!(took && !left, "an image is taken; plain text is left for the message box to paste");
    rig.settle(cx);
    rig.click(cx, ("detach", 0usize));
    assert_eq!(rig.shell.read_with(cx, |s, _| s.attachments.iter().map(|a| a.name.clone()).collect::<Vec<_>>()), ["Pasted image 1.png"]);

    rig.click(cx, "send");
    match rig.asked().as_slice() {
        [Cmd::Send { attachments, text, .. }] => {
            assert_eq!((attachments.len(), attachments[0].media_type.as_str(), attachments[0].data.as_str()), (1, "image/png", "CQk="));
            assert!(text.contains("main.rs"));
        }
        other => panic!("{other:?}"),
    }
    assert!(rig.shell.read_with(cx, |s, _| s.attachments.is_empty()), "sent, so the next message starts clean");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn the_context_meter_appears_once_the_agent_has_said_how_full_it_is(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    assert!(!rig.exists(cx, "context"), "nothing is claimed before it is known");
    let known = SessionSummary { context_used: Some(118_204), context_window: Some(1_000_000), ..idle };
    rig.feed(cx, vec![UiEvent::Session(known)]);
    assert!(rig.exists(cx, "context"));
}

// ---- the transcript and the sidebar -----------------------------------------------

#[gpui_kit::test]
fn a_finished_thought_is_folded_and_opens_on_a_click(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let thought = Item {
        id: ItemId::from("th1"), parent: None, seq: 1, order: 1, created_ms: 0,
        body: ItemBody::Thinking { text: "First I will read the test. Then I will run it. Then I will see what fails.".into(), streaming: false },
    };
    rig.feed(cx, vec![snapshot_of(idle, vec![thought], vec![])]);
    assert!(rig.exists(cx, eid("thought-th1")), "the agent's reasoning is in the transcript");
    assert!(rig.shell.read_with(cx, |s, _| !s.thought_is_open(&ItemId::from("th1"))));
    rig.click(cx, eid("thought-th1"));
    assert!(rig.shell.read_with(cx, |s, _| s.thought_is_open(&ItemId::from("th1"))));
    rig.click(cx, eid("thought-th1"));
    assert!(rig.shell.read_with(cx, |s, _| !s.thought_is_open(&ItemId::from("th1"))), "and folds again");
}

#[gpui_kit::test]
fn subagents_show_under_their_session_and_take_you_to_it(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    let mut busy = session(1, "Refactor auth", RunState::Working, None);
    busy.tasks = vec![
        TaskSummary { id: "a".into(), description: "audit the login flow".into(), background: false, item: None },
        TaskSummary { id: "b".into(), description: "run the test suite".into(), background: true, item: None },
    ];
    rig.feed(cx, vec![UiEvent::Sessions(vec![busy.clone(), session(2, "Docs", RunState::Idle, None)])]);
    let (a, b) = (eid(format!("sub-{}-a", busy.id)), eid(format!("sub-{}-b", busy.id)));
    assert!(rig.exists(cx, a.clone()) && rig.exists(cx, b.clone()));
    rig.asked();
    rig.click(cx, a.clone());
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == busy.id));

    // when they finish their lines go, but not from under a pointer that is resting on the list.
    busy.tasks.clear();
    rig.feed(cx, vec![UiEvent::Session(busy)]);
    assert!(rig.exists(cx, a.clone()), "held in place while the pointer is here");
    cx.update_window(rig.window, |_, window, cx| window.hover("toolbar-overview", cx)).unwrap();
    rig.settle(cx);
    assert!(!rig.exists(cx, a) && !rig.exists(cx, b));
}

// ---- the layout file ---------------------------------------------------------

/// a layout whose sidebar starts shut: the default's panes, with a closed drawer.
const SIDEBAR_SHUT: &str = r#"{"root": {"type": "row", "w": "full", "h": "full", "align": "start", "children": [
    {"type": "drawer", "id": "sidebar", "open": false, "width": 264, "min": 220, "max": 380, "children": [
        {"type": "row", "drag": true, "h": 52}, {"type": "sessions"}]},
    {"type": "panes", "grow": true, "h": "full", "p": 8, "layout": {"split": "row", "children": [
        {"module": "conversation", "size": 3},
        {"id": "inspector", "module": "workspace", "size": 2, "open": false}]}}
]}}"#;

/// a folder of its own for a test's layout file.
fn layout_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("tessel-ui-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[gpui_kit::test]
fn a_layout_decides_what_is_on_screen_and_drawers_work_wherever_they_are(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    assert!(rig.exists(cx, "toolbar-inspector"));

    // a layout whose sidebar starts shut: a drawer that was open stays open across it.
    rig.layout(cx, SIDEBAR_SHUT);
    assert!(rig.exists(cx, sid(2).0), "a drawer that was open stays open across a new layout");
    rig.press(cx, "alt-cmd-s");
    assert!(!rig.exists(cx, sid(2).0));
    rig.click(cx, "toolbar-sidebar");
    assert!(rig.exists(cx, sid(2).0));
    // the workspace pane is folded away, and its key opens it.
    assert!(!rig.exists(cx, "tab-changes"));
    rig.press(cx, "alt-cmd-i");
    assert!(rig.exists(cx, "tab-changes"));

    // a layout of my own: a stop button that exists only while the agent works.
    rig.layout(
        cx,
        r#"{"root": {"type": "column", "children": [
            {"type": "button", "id": "my-stop", "action": "stop", "icon": "square", "when": "busy"},
            {"type": "panel", "tabs": ["checks", "info"]},
            {"type": "transcript", "grow": true},
            {"type": "input"}
        ]}}"#,
    );
    assert!(!rig.exists(cx, "my-stop"));
    assert!(!rig.exists(cx, sid(2).0), "no sessions list in this layout");
    assert!(rig.exists(cx, "tab-checks") && rig.exists(cx, "tab-info") && !rig.exists(cx, "tab-changes"), "only the tabs asked for");
    let busy = session(1, "Fix login", RunState::Working, None);
    rig.feed(cx, vec![UiEvent::Session(busy)]);
    assert!(rig.exists(cx, "my-stop"));
    rig.asked();
    rig.click(cx, "my-stop");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Interrupt(id)] if *id == idle.id));
}

#[gpui_kit::test]
fn the_layout_file_is_watched_and_a_broken_one_keeps_the_last_that_worked(cx: &mut TestAppContext) {
    let dir = layout_dir("watch");
    let file = dir.join("ui.json");
    std::fs::write(&file, SIDEBAR_SHUT).unwrap();
    let mut rig = Rig::launched(cx, Launch { ui: Some(file.clone()), ..Launch::default() });
    let sessions = vec![session(1, "Fix login", RunState::Idle, None), session(2, "Split the auth module", RunState::Idle, None)];
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(sessions)]);
    assert!(!rig.exists(cx, sid(2).0), "this layout starts with the sidebar shut");
    assert!(!rig.exists(cx, "ui-error"));

    // a mistake: said over the window, and the window carries on as it was.
    std::fs::write(&file, r#"{"root": {"type": "colum"}}"#).unwrap();
    rig.settle(cx);
    assert!(rig.exists(cx, "ui-error"));
    assert!(rig.exists(cx, "toolbar-overview") && !rig.exists(cx, sid(2).0), "the last layout that worked is still up");

    // fixed: the error goes, the new layout comes, and the sidebar is still as it was left.
    std::fs::write(&file, r#"{"theme": {"appearance": "dark"}}"#).unwrap();
    rig.settle(cx);
    assert!(!rig.exists(cx, "ui-error"));
    assert!(!rig.exists(cx, sid(2).0), "the sidebar keeps the state it had");
    assert!(rig.shell.read_with(cx, |s, _| s.palette.get().dark), "the theme came with it");
    rig.press(cx, "alt-cmd-s");
    assert!(rig.exists(cx, sid(2).0));

    // no file is the default layout, with nothing to complain about.
    std::fs::remove_file(&file).unwrap();
    rig.settle(cx);
    assert!(!rig.exists(cx, "ui-error") && rig.exists(cx, sid(2).0));
    assert!(!rig.shell.read_with(cx, |s, _| s.palette.get().dark), "and the default theme");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn settings_open_from_the_keyboard_and_write_what_you_pick_to_the_layout_file(cx: &mut TestAppContext) {
    let dir = layout_dir("settings");
    let file = dir.join("ui.json");
    std::fs::write(&file, r#"{"theme": {"font": "Inter"}}"#).unwrap();
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::launched(cx, Launch { ui: Some(file.clone()), ..Launch::default() });
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![idle])]);

    rig.press(cx, "cmd-,");
    assert!(rig.exists(cx, "settings-page"));
    let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();

    // hovering a theme tries it on without writing anything; clicking keeps it.
    rig.click(cx, "settings-tab-themes");
    // the gallery is below how the window is built: scroll to it.
    cx.update_window(rig.window, |_, window, cx| window.scroll("settings-body", gpui_kit::ScrollDelta::Pixels(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-700.))), cx)).unwrap();
    rig.settle(cx);
    let before = read();
    cx.update_window(rig.window, |_, window, cx| window.hover("theme-dusk", cx)).unwrap();
    rig.settle(cx);
    assert_eq!(rig.shell.read_with(cx, |s, _| s.theme_preview_for_test()), Some("dusk".to_owned()));
    assert_eq!(read(), before, "a preview writes nothing");
    rig.click(cx, "theme-orchard");
    assert_eq!(read()["theme"]["base"], "orchard");
    assert_eq!(read()["theme"]["font"], "Inter", "what wasn't touched stays");
    rig.feed(cx, vec![UiEvent::Call { call: 90, cmd: "theme.density".into(), args: serde_json::json!({"density": "compact"}) }]);
    assert_eq!(read()["theme"]["density"], "compact", "as a command, like every setting");
    rig.click(cx, "settings-tab-appearance");
    rig.click(cx, "appearance-dark");
    assert!(rig.shell.read_with(cx, |s, _| s.palette.get().dark), "applied at once, not on the next poll");

    rig.click(cx, "settings-tab-conversation");
    rig.click(cx, "messages-left");
    rig.click(cx, "tools-failed");
    assert_eq!(read()["transcript"], serde_json::json!({"user": "left", "tools": "failed"}));

    rig.click(cx, "settings-tab-appearance");
    rig.click(cx, "accent-teal");
    assert_eq!(read()["theme"]["light"]["accent"], "#1f9eb8");
    rig.click(cx, "accent-default");
    assert!(read()["theme"].get("light").is_none(), "picking the theme's own accent takes the override out");

    // a layout of the file's own is put back to the default, keeping the theme.
    std::fs::write(&file, format!(r#"{{"theme": {{"base": "orchard"}}, "root": {}}}"#, &SIDEBAR_SHUT[9..SIDEBAR_SHUT.len() - 1])).unwrap();
    rig.settle(cx);
    rig.click(cx, "settings-tab-customize");
    rig.click(cx, "settings-reset-layout");
    assert!(read().get("root").is_none() && read()["theme"]["base"] == "orchard");

    // starting over takes two presses, and deletes the file.
    rig.click(cx, "settings-reset");
    assert!(file.exists());
    rig.click(cx, "settings-reset");
    assert!(!file.exists());

    rig.press(cx, "escape");
    assert!(!rig.exists(cx, "settings-page"));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn the_new_session_form_picks_in_place_or_a_worktree(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![session(1, "Fix login", RunState::Idle, None)])]);
    rig.asked();
    rig.press(cx, "cmd-n");
    rig.click(cx, "worktree-toggle");
    rig.click(cx, "in-place");
    rig.click(cx, "new-prompt");
    rig.typed(cx, "tidy the readme");
    rig.click(cx, "create");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Create { worktree: false, prompt, cwd }] if prompt == "tidy the readme" && cwd == "/work/api"), "the latest project, unasked");
}

#[gpui_kit::test]
fn the_model_and_mode_pickers_open_above_the_message_box_and_pick(cx: &mut TestAppContext) {
    use tessel_proto::FavoriteModel;
    let mut idle = session(1, "Fix login", RunState::Idle, None);
    idle.model = Some("claude-sonnet-5-5".into());
    idle.permission_mode = Some("default".into());
    let efforts: Vec<String> = ["low", "medium", "high", "xhigh", "max"].map(String::from).to_vec();
    let model = |value: &str, id: &str, name: &str, fast: bool| ModelInfo { value: value.into(), id: id.into(), name: name.into(), description: "x".into(), efforts: efforts.clone(), fast };
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.feed(
        cx,
        vec![
            UiEvent::Models(vec![model("opus", "claude-opus-5-5", "Opus 5.5", true), model("sonnet", "claude-sonnet-5-5", "Sonnet 5.5", false), model("claude-opus-4-7", "claude-opus-4-7", "Opus 4.7", false)], vec![]),
            UiEvent::DaemonSettings(tessel_proto::DaemonSettings {
                agent_tools: true,
                models: vec![FavoriteModel { model: "opus".into(), effort: Some("max".into()), fast: true }, FavoriteModel { model: "sonnet".into(), effort: Some("low".into()), fast: false }],
            }),
        ],
    );
    rig.click(cx, "model-menu");
    assert!(rig.exists(cx, "model-search"), "the picker opens with its search");
    let (menu, trigger) = cx.update_window(rig.window, |_, window, _| (window.find("model-list").bounds(), window.find("model-menu").bounds())).unwrap();
    assert!(menu.origin.y + menu.size.height <= trigger.origin.y + gpui_kit::px(1.), "it opens above the button: {menu:?} {trigger:?}");
    assert!(rig.exists(cx, "favorite-0") && rig.exists(cx, "favorite-1") && !rig.exists(cx, "favorite-2"), "the loadout, and only it");
    assert!(!rig.exists(cx, "model-claude-opus-4-7"), "the rest wait for a search");
    rig.asked();

    // a favourite brings its effort and its speed with it.
    rig.click(cx, "favorite-0");
    match rig.asked().as_slice() {
        [Cmd::SetModel { model, .. }, Cmd::SetEffort { effort, .. }, Cmd::SetFast { fast, .. }] => assert_eq!((model.as_str(), effort.as_deref(), *fast), ("opus", Some("max"), true)),
        other => panic!("{other:?}"),
    }
    assert!(!rig.exists(cx, "model-search"), "picking closes it");

    // a search finds every model.
    rig.click(cx, "model-menu");
    rig.typed(cx, "4.7");
    assert!(rig.exists(cx, "model-claude-opus-4-7") && !rig.exists(cx, "favorite-0"));
    rig.click(cx, "model-claude-opus-4-7");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetModel { model, .. }] if model == "claude-opus-4-7"));

    // the session's effort, from the picker's tray.
    rig.click(cx, "model-menu");
    rig.click(cx, "effort-high");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetEffort { effort: Some(e), .. }] if e == "high"));
    assert!(!rig.exists(cx, "model-fast"), "sonnet has no fast mode, so no switch");

    // the keys: a favourite by its place, the next effort up, fast on and off.
    rig.press(cx, "escape");
    rig.press(cx, "ctrl-cmd-2");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetModel { model, .. }, Cmd::SetEffort { effort: Some(e), .. }, Cmd::SetFast { fast: false, .. }] if model == "sonnet" && e == "low"));
    let mut medium = idle.clone();
    medium.effort = Some("medium".into());
    rig.feed(cx, vec![UiEvent::Sessions(vec![medium])]);
    rig.press(cx, "shift-cmd-e");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetEffort { effort: Some(e), .. }] if e == "high"), "medium goes up to high");
    let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec("model.fast", &serde_json::json!({}), window, cx))).unwrap();
    assert!(r.is_err(), "sonnet can't run fast: {r:?}");
    let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec("model.favorite", &serde_json::json!({"n": 3}), window, cx))).unwrap();
    assert!(r.is_err(), "there is no third favourite");

    rig.click(cx, "mode-menu");
    rig.click(cx, "mode-plan");
    assert!(matches!(rig.asked().as_slice(), [.., Cmd::SetMode { mode, .. }] if mode == "plan"));
}

#[gpui_kit::test]
fn the_models_page_keeps_the_loadout(cx: &mut TestAppContext) {
    use tessel_proto::FavoriteModel;
    let idle = session(1, "Fix login", RunState::Idle, None);
    let efforts: Vec<String> = ["low", "medium", "high"].map(String::from).to_vec();
    let model = |value: &str, name: &str, fast: bool| ModelInfo { value: value.into(), id: value.into(), name: name.into(), description: "x".into(), efforts: efforts.clone(), fast };
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.feed(
        cx,
        vec![
            UiEvent::Models(vec![model("opus", "Opus 5.5", true), model("haiku", "Haiku 5.5", false)], vec![]),
            UiEvent::DaemonSettings(tessel_proto::DaemonSettings { agent_tools: true, models: vec![FavoriteModel { model: "opus".into(), effort: Some("high".into()), fast: false }] }),
        ],
    );
    rig.shell.update(cx, |s, cx| {
        s.settings_section = "models";
        s.open_settings(cx);
    });
    rig.settle(cx);
    rig.asked();
    rig.click(cx, "models-opus-low");
    let saved = |rig: &mut Rig| {
        rig.asked().into_iter().find_map(|c| match c {
            Cmd::SetSettings(v) => serde_json::from_value::<Vec<FavoriteModel>>(v["models"].clone()).ok(),
            _ => None,
        })
    };
    assert_eq!(saved(&mut rig).unwrap()[0].effort.as_deref(), Some("low"), "a favourite's effort is kept by the daemon");
    rig.click(cx, "models-opus-fast");
    assert!(saved(&mut rig).unwrap()[0].fast);
    rig.click(cx, "models-haiku-star");
    let favs = saved(&mut rig).unwrap();
    assert_eq!(favs.iter().map(|f| f.model.as_str()).collect::<Vec<_>>(), ["opus", "haiku"], "a star adds it at the end");
}

#[gpui_kit::test]
fn a_working_agent_says_so_above_the_message_box_and_an_idle_one_doesnt(cx: &mut TestAppContext) {
    let mut busy = session(1, "Fix login", RunState::Working, None);
    busy.activity = Some("running the tests".into());
    busy.activity_since_ms = Some(1);
    let mut rig = Rig::with_sessions(cx, busy.clone(), vec![]);
    assert!(rig.exists(cx, "working"), "the dot and what it's doing, where you're looking");
    let mut idle = busy;
    idle.run_state = RunState::Idle;
    rig.feed(cx, vec![UiEvent::Session(idle)]);
    assert!(!rig.exists(cx, "working"));
}

#[gpui_kit::test]
fn the_sidebars_session_list_counts_so_a_pane_cant_add_a_second(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec("pane.show", &serde_json::json!({"module": "sessions"}), window, cx))).unwrap();
    assert!(r.is_err(), "the sidebar has the list already: {r:?}");
    rig.click(cx, "pane-conversation-more");
    assert!(rig.exists(cx, "pane-conversation-show-terminal") && !rig.exists(cx, "pane-conversation-show-sessions"), "and the menu doesn't offer it");
}

#[gpui_kit::test]
fn a_pane_takes_whatever_share_it_is_given(cx: &mut TestAppContext) {
    let dir = layout_dir("pane-size");
    let file = dir.join("ui.json");
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, idle, vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let exec = |rig: &mut Rig, cx: &mut TestAppContext, cmd: &str, args: serde_json::Value| cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec(cmd, &args, window, cx))).unwrap();
    let new = exec(&mut rig, cx, "pane.split", serde_json::json!({"pane": "conversation", "dir": "right"})).unwrap();
    let new = new["pane"].as_str().unwrap().to_owned();
    exec(&mut rig, cx, "pane.size", serde_json::json!({"pane": "conversation", "size": 0.75})).unwrap();
    let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let f = read();
    let mut sizes = std::collections::HashMap::new();
    fn walk(v: &serde_json::Value, out: &mut std::collections::HashMap<String, f64>) {
        if let Some(id) = v.get("id").and_then(|i| i.as_str()).or_else(|| v.get("module").and_then(|m| m.as_str())) {
            out.insert(id.to_owned(), v.get("size").and_then(|s| s.as_f64()).unwrap_or(1.0));
        }
        for c in v.get("children").and_then(|c| c.as_array()).into_iter().flatten() {
            walk(c, out);
        }
        if let Some(l) = v.get("layout") {
            walk(l, out);
        }
    }
    walk(&f["root"], &mut sizes);
    let (a, b) = (sizes["conversation"], sizes[&new]);
    assert!((a / (a + b) - 0.75).abs() < 0.01, "three quarters of the room, the hidden inspector not counted: {sizes:?}");
    // a preset, and an axis there's no split along.
    assert!(exec(&mut rig, cx, "pane.size", serde_json::json!({"pane": "conversation", "size": "half"})).is_ok());
    assert!(exec(&mut rig, cx, "pane.size", serde_json::json!({"pane": "conversation", "size": 0.5, "dir": "height"})).is_err(), "nothing above or below it");
    assert!(exec(&mut rig, cx, "pane.size", serde_json::json!({"pane": "conversation", "size": 1.4})).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn a_new_session_gets_a_worktree_unless_its_folder_isnt_a_repository(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![session(1, "Fix login", RunState::Idle, None)])]);
    rig.asked();
    rig.press(cx, "cmd-n");
    rig.click(cx, "new-prompt");
    rig.typed(cx, "tidy up");
    // the daemon says /work/api is a plain folder.
    rig.feed(cx, vec![UiEvent::Answer { tag: "folder.info", result: Ok(Response::FolderInfo { git_root: None }) }]);
    rig.click(cx, "create");
    let asked = rig.asked();
    let known = rig.shell.read_with(cx, |s, _| format!("{:?}", s.folder_git.borrow()));
    assert!(matches!(asked.as_slice(), [Cmd::Create { worktree: false, .. }]), "a plain folder works in place: {asked:?} {known}");
    // a repository: a workspace of its own, unasked.
    rig.shell.update(cx, |s, _| {
        s.folder_git.borrow_mut().insert("/work/api".into(), Some(true));
    });
    rig.press(cx, "cmd-n");
    rig.click(cx, "new-prompt");
    rig.typed(cx, "and again");
    rig.click(cx, "create");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Create { worktree: true, .. }]));
}

#[gpui_kit::test]
fn the_terminal_look_is_set_from_a_command_and_kept_in_the_file(cx: &mut TestAppContext) {
    let dir = layout_dir("terminal-look");
    let file = dir.join("ui.json");
    let mut rig = Rig::launched(cx, Launch { ui: Some(file.clone()), ..Launch::default() });
    let exec = |rig: &mut Rig, cx: &mut TestAppContext, args: serde_json::Value| cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec("terminal.look", &args, window, cx))).unwrap();
    exec(&mut rig, cx, serde_json::json!({"size": 15, "colors": "ink"})).unwrap();
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!((f["theme"]["terminal"]["size"].as_f64(), f["theme"]["terminal"]["colors"].as_str()), (Some(15.0), Some("ink")));
    rig.settle(cx);
    assert_eq!(rig.shell.read_with(cx, |s, _| s.ui.theme.term_colors.clone()).as_deref(), Some("ink"), "the window follows the file");
    exec(&mut rig, cx, serde_json::json!({"colors": "theme"})).unwrap();
    let f: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(f["theme"]["terminal"].get("colors").is_none(), "theme is the default, so it isn't written");
    assert!(exec(&mut rig, cx, serde_json::json!({"size": 3})).is_err());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn a_turn_ends_with_its_time_its_reply_and_the_files_it_changed(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let it = |id: &str, seq: u64, body: ItemBody| Item { id: ItemId::from(id), parent: None, seq, order: seq, created_ms: 0, body };
    let file = |p: &str, a: u32, r: u32| tessel_proto::TurnFile { path: p.into(), added: a, removed: r };
    let items = vec![
        it("u1", 1, ItemBody::User { text: "tidy the readme".into(), attachments: vec![] }),
        it("t1", 2, ItemBody::Text { text: "done, the readme is tidy".into(), streaming: false }),
        it("turn/3", 3, ItemBody::Turn {
            started_ms: 1_000,
            finished_ms: 45_000,
            duration_ms: Some(44_000),
            cost_usd: Some(0.3),
            model: Some("claude-opus-5-5".into()),
            fast: false,
            usage: Some(tessel_proto::TurnUsage { input: 12, output: 3780, cache_read: 4_159_013, cache_write: 4356 }),
            files: vec![file("README.md", 3, 3), file("docs/a.md", 1, 0), file("docs/b.md", 2, 1)],
        }),
    ];
    rig.feed(cx, vec![snapshot_of(idle, items, vec![])]);
    assert!(rig.exists(cx, "turn-turn/3-usage"), "the time, which opens what it took");
    assert!(rig.exists(cx, "turn-turn/3-file-0") && rig.exists(cx, "turn-turn/3-file-1"), "two files by name");
    assert!(rig.exists(cx, "turn-turn/3-file-more") && !rig.exists(cx, "turn-turn/3-file-2"), "and the rest as one");
    assert_eq!(rig.shell.read_with(cx, |s, _| s.turn_text_for_test(2)), "done, the readme is tidy", "copy takes the reply");
}

#[gpui_kit::test]
fn a_workspaces_sessions_are_its_tabs_and_a_turn_forks_into_a_new_one(cx: &mut TestAppContext) {
    let a = session(1, "Fix login", RunState::Idle, None);
    // a second tab in a's workspace.
    let b = SessionSummary { cwd: a.cwd.clone(), ..session(2, "Write tests", RunState::Working, None) };
    let mut elsewhere = session(3, "Other repo", RunState::Idle, None);
    elsewhere.cwd = "/work/web".into();
    let mut rig = Rig::with_sessions(cx, a.clone(), vec![]);
    rig.feed(cx, vec![UiEvent::Sessions(vec![a.clone(), b.clone(), elsewhere.clone()])]);
    let tab = |s: &SessionSummary| SharedString::from(format!("tab-{}", s.id));
    assert!(rig.exists(cx, tab(&a)) && rig.exists(cx, tab(&b)), "the same folder: tabs of one workspace");
    assert!(!rig.exists(cx, tab(&elsewhere)), "another folder is another workspace");
    rig.asked();
    rig.click(cx, tab(&b));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == b.id));
    rig.feed(cx, vec![snapshot(b.clone(), vec![])]);

    // + is a new chat at once: the same folder, in place, no form.
    rig.click(cx, "tab-new");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Create { cwd, worktree: false, prompt }] if *cwd == a.cwd && prompt.is_empty()));
    assert!(!rig.exists(cx, "create"), "no new-session form");

    // a turn's menu forks the conversation.
    let it = |id: &str, seq: u64, body: ItemBody| Item { id: ItemId::from(id), parent: None, seq, order: seq, created_ms: 0, body };
    let turn = ItemBody::Turn { started_ms: 0, finished_ms: 1, duration_ms: None, cost_usd: None, model: None, fast: false, usage: None, files: vec![] };
    rig.shell.update(cx, |s, cx| s.open(a.id, cx));
    rig.feed(cx, vec![snapshot_of(a.clone(), vec![it("u1", 1, ItemBody::User { text: "hi".into(), attachments: vec![] }), it("turn/2", 2, turn)], vec![])]);
    rig.asked();
    rig.click(cx, "turn-turn/2-more");
    rig.click(cx, "turn-fork-workspace");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Fork { session, workspace: true }] if *session == a.id));
}

#[gpui_kit::test]
fn a_tab_closes_an_empty_chat_at_once_and_asks_before_losing_history(cx: &mut TestAppContext) {
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut b = SessionSummary { cwd: a.cwd.clone(), ..session(2, "untitled", RunState::Idle, None) };
    b.cost_usd = 0.0;
    let c = SessionSummary { cwd: a.cwd.clone(), ..session(3, "Write tests", RunState::Idle, None) };
    let mut rig = Rig::with_sessions(cx, a.clone(), vec![]);
    rig.feed(cx, vec![UiEvent::Sessions(vec![a.clone(), b.clone(), c.clone()])]);
    rig.asked();
    let close = |s: &SessionSummary| SharedString::from(format!("tab-close-{}", s.id));

    // nothing in it: gone on the first click, and the tab it was on stays open.
    // a background tab's cross shows when you point at the tab.
    let tab_b = SharedString::from(format!("tab-{}", b.id));
    cx.update_window(rig.window, |_, window, cx| window.hover(tab_b, cx)).unwrap();
    rig.click(cx, close(&b));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Delete { session, .. }] if *session == b.id), "an empty chat goes at once");
    rig.feed(cx, vec![UiEvent::Removed(b.id)]);
    assert!(!rig.exists(cx, SharedString::from(format!("tab-{}", b.id))));

    // history: the first click only asks.
    rig.click(cx, close(&a));
    assert!(rig.asked().is_empty(), "one stray click doesn't lose a conversation");
    rig.click(cx, close(&a));
    assert!(matches!(rig.asked().as_slice(), [Cmd::Delete { session, .. }] if *session == a.id));

    // the open tab gone, the one beside it opens.
    rig.feed(cx, vec![UiEvent::Removed(a.id)]);
    assert!(matches!(rig.asked().as_slice(), [Cmd::Open(Some(id))] if *id == c.id), "its neighbour, not the new-session form");
}

#[gpui_kit::test]
fn the_changes_list_shows_all_uncommitted_this_session_s_or_the_last_turn_s(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let it = |id: &str, seq: u64, body: ItemBody| Item { id: ItemId::from(id), parent: None, seq, order: seq, created_ms: 0, body };
    let turn = |files: Vec<&str>| ItemBody::Turn {
        started_ms: 0, finished_ms: 1, duration_ms: None, cost_usd: None, model: None, fast: false, usage: None,
        files: files.into_iter().map(|p| tessel_proto::TurnFile { path: p.into(), added: 1, removed: 0 }).collect(),
    };
    rig.feed(cx, vec![snapshot_of(idle.clone(), vec![it("turn/1", 1, turn(vec!["a.rs"])), it("turn/2", 2, turn(vec!["b.rs"]))], vec![])]);
    rig.press(cx, "alt-cmd-i");
    let files = vec![change("a.rs", ChangeStatus::Modified, 1, 0), change("b.rs", ChangeStatus::Modified, 1, 0), change("c.rs", ChangeStatus::Modified, 1, 0)];
    rig.feed(cx, vec![UiEvent::Git { session: idle.id, summary: git(files) }]);
    let shown = |rig: &mut Rig, cx: &mut TestAppContext| ["a.rs", "b.rs", "c.rs"].map(|f| rig.exists(cx, eid(&format!("change-{f}"))));
    assert_eq!(shown(&mut rig, cx), [true, true, true], "everything uncommitted");
    rig.click(cx, "changes-scope-session");
    assert_eq!(shown(&mut rig, cx), [true, true, false], "what this session's turns changed");
    rig.click(cx, "changes-scope-turn");
    assert_eq!(shown(&mut rig, cx), [false, true, false], "the last turn's");
    rig.click(cx, "changes-scope-all");
    rig.click(cx, "changes-search");
    rig.typed(cx, "c.r");
    assert_eq!(shown(&mut rig, cx), [false, false, true], "and narrowed by name");
}

#[gpui_kit::test]
fn every_shortcut_the_window_shows_is_bound(cx: &mut TestAppContext) {
    let _rig = Rig::new(cx);
    // "⇧ ⌘ D" as modifiers and a key, the way the keymap holds it.
    let parse = |caps: &str| {
        let mut m = (false, false, false, false, String::new());
        for k in caps.split_whitespace() {
            match k {
                "⌃" => m.0 = true,
                "⌥" => m.1 = true,
                "⇧" => m.2 = true,
                "⌘" => m.3 = true,
                "↩" => m.4 = "enter".into(),
                "←" => m.4 = "left".into(),
                "→" => m.4 = "right".into(),
                k => m.4 = k.to_lowercase(),
            }
        }
        m
    };
    let bound: Vec<(bool, bool, bool, bool, String)> = cx.update(|cx| {
        cx.key_bindings()
            .borrow()
            .bindings()
            .filter(|b| b.keystrokes().len() == 1)
            .map(|b| {
                let k = &b.keystrokes()[0];
                let m = k.modifiers();
                (m.control, m.alt, m.shift, m.platform, k.key().to_owned())
            })
            .collect()
    });
    for spec in crate::commands::COMMANDS {
        if let Some(caps) = spec.keys {
            assert!(bound.contains(&parse(caps)), "{} shows {caps} but nothing is bound to it", spec.id);
        }
    }
}

#[gpui_kit::test]
fn the_backdrop_switches_on_from_settings_and_says_so_in_the_file(cx: &mut TestAppContext) {
    let dir = layout_dir("backdrop");
    let file = dir.join("ui.json");
    let mut rig = Rig::launched(cx, Launch { ui: Some(file.clone()), ..Launch::default() });
    assert!(!rig.shell.read_with(cx, |s, _| s.backdrop_on()), "off by default");
    rig.shell.update(cx, |s, cx| {
        s.settings_section = "themes";
        s.open_settings(cx);
    });
    rig.settle(cx);
    rig.click(cx, "backdrop-dither");
    let written: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(written["theme"]["backdrop"], "dither");
    rig.settle(cx);
    assert!(rig.shell.read_with(cx, |s, _| s.backdrop_on()), "and the window follows the file");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn menus_float_on_the_raised_colour_not_the_librarys_black(cx: &mut TestAppContext) {
    let rig = Rig::new(cx);
    let raised = rig.shell.read_with(cx, |s, _| s.palette.get().raised);
    let popover = cx.update(|cx| gpui_kit::component::Theme::global(cx).colors.popover);
    assert_eq!(popover, raised);
}

#[gpui_kit::test]
fn a_long_settings_page_scrolls_to_its_end(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    let models: Vec<ModelInfo> = (0..14).map(|i| ModelInfo { value: format!("m{i}"), id: format!("m{i}"), name: format!("Model {i}"), description: "x".into(), ..ModelInfo::default() }).collect();
    rig.feed(cx, vec![UiEvent::Models(models, vec![])]);
    rig.shell.update(cx, |s, cx| {
        s.settings_section = "models";
        s.open_settings(cx);
    });
    rig.settle(cx);
    let bottoms = |rig: &mut Rig, cx: &mut TestAppContext| {
        cx.update_window(rig.window, |_, window, _| {
            let (body, last) = (window.find("settings-body").bounds(), window.find("models-row-m13").bounds());
            (body.origin.y + body.size.height, last.origin.y + last.size.height)
        })
        .unwrap()
    };
    let (body, last) = bottoms(&mut rig, cx);
    assert!(last > body, "fourteen models run past the page: {last:?} {body:?}");
    cx.update_window(rig.window, |_, window, cx| window.scroll("settings-body", gpui_kit::ScrollDelta::Pixels(gpui_kit::point(gpui_kit::px(0.), gpui_kit::px(-5000.))), cx)).unwrap();
    rig.settle(cx);
    let (body, last) = bottoms(&mut rig, cx);
    assert!((last - body).abs() < gpui_kit::px(40.), "and scroll to the last one: {last:?} {body:?}");
}

#[gpui_kit::test]
fn slash_opens_the_agents_commands_and_return_takes_one(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    let cmd = |name: &str, description: &str| tessel_proto::SlashCommand { name: name.into(), description: description.into(), hint: String::new() };
    rig.feed(cx, vec![UiEvent::Models(vec![], vec![cmd("review", "Review a pull request"), cmd("compact", "Summarise the history")])]);
    rig.click(cx, "composer");
    rig.typed(cx, "/rev");
    assert!(rig.exists(cx, "mention-0") && !rig.exists(cx, "mention-1"), "only what matches");
    rig.asked();
    rig.press(cx, "enter");
    assert!(rig.asked().is_empty(), "return takes the command; it doesn't send");
    assert_eq!(rig.shell.read_with(cx, |s, cx| s.composer_text(cx)), "/review ");
    assert!(!rig.exists(cx, "mention-0"), "and the menu closes");
    rig.typed(cx, "42");
    rig.press(cx, "enter");
    let asked = rig.asked();
    assert!(matches!(asked.as_slice(), [Cmd::Send { text, .. }] if text == "/review 42"), "{asked:?}");
}

// ---- panes ----------------------------------------------------------------------

#[gpui_kit::test]
fn panes_split_take_a_module_and_close_and_the_file_keeps_what_you_did(cx: &mut TestAppContext) {
    let dir = layout_dir("panes");
    let file = dir.join("ui.json");
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, idle, vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();
    let hover = |rig: &mut Rig, cx: &mut TestAppContext, id: &'static str| {
        cx.update_window(rig.window, |_, window, cx| window.hover(id, cx)).unwrap();
        rig.settle(cx);
    };

    // split the conversation: an empty pane appears beside it, offering modules.
    hover(&mut rig, cx, "pane-conversation-header");
    rig.click(cx, "pane-conversation-more");
    rig.click(cx, "pane-conversation-split-right");
    let layout = &read()["root"]["children"][1]["layout"];
    assert_eq!(layout["children"].as_array().unwrap().len(), 3, "a sibling in the same row: {layout}");
    assert!(rig.exists(cx, "add-changes"), "the empty pane offers modules");

    // fill it with the changes module.
    rig.click(cx, "add-changes");
    assert_eq!(read()["root"]["children"][1]["layout"]["children"][1]["module"], "changes");
    assert!(rig.shell.read_with(cx, |s, _| s.visible_tabs().contains(&crate::shell::Tab::Changes)), "and it is kept up to date");

    // and close it again. it kept the name the split gave it.
    hover(&mut rig, cx, "pane-pane-1-header");
    rig.click(cx, "pane-pane-1-more");
    rig.click(cx, "pane-pane-1-close");
    let layout = &read()["root"]["children"][1]["layout"];
    assert_eq!(layout["children"].as_array().unwrap().len(), 2, "{layout}");
    assert!(!rig.exists(cx, "pane-pane-1-header"), "gone once it has faded");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn a_layout_changed_from_outside_moves_the_panes_rather_than_jumping(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.press(cx, "alt-cmd-i");
    let width = |rig: &Rig, cx: &mut TestAppContext| cx.update_window(rig.window, |_, window, _| window.find("pane-conversation-header").bounds().size.width).unwrap();
    let before = width(&rig, cx);
    // an agent gives the conversation all the room.
    let shell = rig.shell.clone();
    cx.update_window(rig.window, |_, window, cx| {
        shell.update(cx, |s, cx| s.use_ui_text(r#"{"root": {"type": "panes", "drag": true, "inset": true, "h": "full", "w": "full", "layout": {"module": "conversation"}}}"#, window, cx))
    })
    .unwrap()
    .unwrap();
    // one frame in, it is on its way, not there.
    cx.run_until_parked();
    cx.executor().advance_clock(std::time::Duration::from_millis(16));
    rig.frame(cx);
    rig.frame(cx);
    let during = width(&rig, cx);
    rig.settle(cx);
    let after = width(&rig, cx);
    assert!(after > before, "it took the room: {before:?} -> {after:?}");
    assert!(during > before && during < after, "and moved through the space between: {before:?} {during:?} {after:?}");
}

#[test]
fn a_subagents_report_is_told_apart_from_what_a_person_wrote() {
    let wrapped = "<agent-message from=\"a17\">\n[Subagent hand-back] The text below is the final report. The report follows:\n  alive and wagging\n  second line\n</agent-message>";
    assert_eq!(crate::shell::handback_for_test(wrapped).as_deref(), Some("alive and wagging\nsecond line"));
    assert_eq!(crate::shell::handback_for_test("just a test"), None);
    assert_eq!(crate::shell::handback_for_test("what does <agent-message> mean?"), None);
}

#[gpui_kit::test]
fn keys_split_and_maximize_the_pane_you_last_clicked_in(cx: &mut TestAppContext) {
    let dir = layout_dir("pane-keys");
    let file = dir.join("ui.json");
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, idle, vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let read = || serde_json::from_str::<serde_json::Value>(&std::fs::read_to_string(&file).unwrap()).unwrap();
    // nothing clicked yet: keys act on the conversation.
    rig.press(cx, "shift-cmd-d");
    let layout = &read()["root"]["children"][1]["layout"]["children"][0];
    assert_eq!(layout["split"], "column", "the conversation split downwards: {layout}");

    fn width(rig: &Rig, cx: &mut TestAppContext, id: &'static str) -> gpui_kit::Pixels {
        cx.update_window(rig.window, |_, window, _| window.find(id).bounds().size.width).unwrap()
    }
    rig.press(cx, "alt-cmd-i");
    // the split took the focus; clicking the conversation brings it back.
    rig.click(cx, "pane-conversation-header");
    // sizes are read once the springs have come to rest.
    let at_rest = |rig: &mut Rig, cx: &mut TestAppContext| {
        rig.settle(cx);
        width(rig, cx, "pane-conversation-header")
    };
    let before = at_rest(&mut rig, cx);
    rig.press(cx, "shift-cmd-enter");
    let maximized = at_rest(&mut rig, cx);
    assert!(maximized > before, "maximized: {before:?} -> {maximized:?}");
    rig.press(cx, "shift-cmd-enter");
    let restored = at_rest(&mut rig, cx);
    assert!((restored - before).abs() < gpui_kit::px(2.), "and back: {before:?} {restored:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn a_window_down_to_one_empty_pane_still_has_a_way_back(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![session(1, "Fix login", RunState::Idle, None)])]);
    rig.layout(cx, r#"{"root": {"type": "row", "w": "full", "h": "full", "children": [
        {"type": "drawer", "id": "sidebar", "open": false, "children": [{"type": "sessions"}]},
        {"type": "panes", "grow": true, "h": "full", "p": 8, "layout": {}}
    ]}}"#);
    // a drawer that was open stays open across a new layout: shut it.
    rig.press(cx, "alt-cmd-s");
    assert!(!rig.exists(cx, sid(1).0), "the sidebar is shut");
    assert!(rig.exists(cx, "empty-reset-layout"), "the only pane offers the default layout back");
    rig.click(cx, "pane-pane-1-sidebar");
    assert!(rig.exists(cx, sid(1).0), "and its corner opens the sidebar");
}

// ---- scripting and the palette ----------------------------------------------------

#[gpui_kit::test]
fn a_script_drives_the_window_through_the_same_commands_and_is_answered(cx: &mut TestAppContext) {
    let dir = layout_dir("script");
    let file = dir.join("ui.json");
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, idle.clone(), vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let answer = |rig: &mut Rig| {
        rig.asked().into_iter().find_map(|c| match c {
            Cmd::UiAnswer { call, result } => Some((call, result)),
            _ => None,
        })
    };

    rig.feed(cx, vec![UiEvent::Call { call: 1, cmd: "ui.ls".into(), args: serde_json::Value::Null }]);
    let (call, result) = answer(&mut rig).expect("answered");
    let ls = result.unwrap();
    assert_eq!(call, 1);
    assert_eq!(ls["session"]["goal"], "Fix login");
    assert!(ls["panes"].as_array().unwrap().iter().any(|p| p["module"] == "conversation" && p["focused"] == true));

    rig.feed(cx, vec![UiEvent::Call { call: 2, cmd: "pane.split".into(), args: serde_json::json!({"dir": "down"}) }]);
    let split = answer(&mut rig).unwrap().1.unwrap();
    let new = split["pane"].as_str().expect("names the new pane").to_owned();
    // "split, then show" fills the new pane rather than replacing the one you were in.
    rig.feed(cx, vec![UiEvent::Call { call: 20, cmd: "pane.show".into(), args: serde_json::json!({"module": "changes"}) }]);
    assert!(answer(&mut rig).unwrap().1.is_ok());
    rig.feed(cx, vec![UiEvent::Call { call: 21, cmd: "ui.ls".into(), args: serde_json::Value::Null }]);
    let ls = answer(&mut rig).unwrap().1.unwrap();
    let panes = ls["panes"].as_array().unwrap();
    assert!(panes.iter().any(|p| p["module"] == "conversation"), "the conversation is still there: {panes:?}");
    assert!(panes.iter().any(|p| p["id"] == new.as_str() && p["module"] == "changes" && p["focused"] == true), "{panes:?}");
    let layout: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(layout["root"]["children"][1]["layout"]["children"][0]["split"], "column", "written like a click would");

    rig.feed(cx, vec![UiEvent::Call { call: 3, cmd: "pane.show".into(), args: serde_json::json!({"module": "nope"}) }]);
    let err = answer(&mut rig).unwrap().1.unwrap_err();
    assert!(err.contains("modules: conversation"), "{err}");
    rig.feed(cx, vec![UiEvent::Call { call: 4, cmd: "pane.splt".into(), args: serde_json::json!({}) }]);
    assert!(answer(&mut rig).unwrap().1.unwrap_err().contains("pane.split"), "suggests what was meant");

    // nobody listening: nothing is published. someone listening: every command says so.
    rig.feed(cx, vec![UiEvent::Call { call: 5, cmd: "theme.set".into(), args: serde_json::json!({"base": "orchard"}) }]);
    assert!(!rig.asked().iter().any(|c| matches!(c, Cmd::UiPublish { .. })));
    rig.feed(cx, vec![UiEvent::Watched(true), UiEvent::Call { call: 6, cmd: "theme.size".into(), args: serde_json::json!({"size": "compact"}) }]);
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::UiPublish { kind, .. } if kind == "command_run")));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn the_palette_finds_a_command_by_a_few_letters_and_runs_it(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.press(cx, "cmd-k");
    assert!(rig.exists(cx, "palette"));
    rig.typed(cx, "sessions");
    assert!(rig.exists(cx, "palette-0"));
    // the session list: the top hit is the "All sessions" command; arrows move, return runs.
    rig.press(cx, "enter");
    assert!(!rig.exists(cx, "palette"), "running closes it");
    assert!(rig.shell.read_with(cx, |s, _| s.overview_for_test()), "and All sessions opened");
    rig.press(cx, "escape");

    // sessions are in it too.
    rig.press(cx, "cmd-k");
    rig.typed(cx, "split the auth");
    rig.press(cx, "enter");
    assert!(matches!(rig.asked().last(), Some(Cmd::Open(Some(id))) if *id == sid(2)));
}

// ---- surfaces ---------------------------------------------------------------------

fn surface_item(name: &str, order: u64, node: serde_json::Value, region: &str) -> Item {
    Item { id: ItemId::from(format!("surface/{name}").as_str()), parent: None, seq: order, order, created_ms: 0, body: ItemBody::Surface { surface: name.into(), node, region: region.into() } }
}

#[gpui_kit::test]
fn an_agents_surface_is_drawn_in_the_chat_and_what_you_pick_goes_back_to_it(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let node = serde_json::json!({"k": "card", "p": {"title": "Deploy"}, "c": [
        {"k": "choice", "id": "env", "p": {"options": ["staging", "production"], "value": "staging"}},
        {"k": "button", "id": "go", "p": {"label": "Promote", "variant": "primary"}}
    ]});
    rig.feed(cx, vec![snapshot_of(idle, vec![surface_item("deploy", 1, node, "inline")], vec![])]);
    assert!(rig.exists(cx, "surface-deploy-go"));

    rig.click(cx, "surface-deploy-env-1");
    rig.click(cx, "surface-deploy-go");
    let sent: Vec<(String, String, serde_json::Value)> = rig
        .asked()
        .into_iter()
        .filter_map(|c| match c {
            Cmd::SurfaceEvent { surface, element, act, value, session } if surface == "deploy" && session == sid(1) => Some((element, act, value)),
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec![("env".into(), "change".into(), serde_json::json!("production")), ("go".into(), "click".into(), serde_json::Value::Null)]);
}

#[gpui_kit::test]
fn a_pane_surface_lives_in_the_surfaces_pane_while_one_is_open_and_in_the_chat_when_not(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    let node = serde_json::json!({"k": "button", "id": "go", "p": {"label": "Go"}});
    rig.feed(cx, vec![snapshot_of(idle, vec![surface_item("s", 1, node, "pane")], vec![])]);
    assert!(rig.exists(cx, "surface-s-go"), "no surfaces pane: drawn in the chat, never lost");

    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "row", "children": [
        {"module": "conversation"}, {"module": "surfaces"}
    ]}}}"#);
    let found = |rig: &mut Rig, cx: &mut TestAppContext| cx.update_window(rig.window, |_, window, _| window.find_all("surface-s-go").len()).unwrap();
    assert_eq!(found(&mut rig, cx), 1, "drawn once");
    assert!(cx.update_window(rig.window, |_, window, _| window.within("surfaces-pane").try_find("surface-s-go").is_some()).unwrap(), "and in the pane");
}

#[gpui_kit::test]
fn agents_can_be_kept_from_tessels_tools_from_settings(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.press(cx, "cmd-,");
    rig.click(cx, "settings-tab-agents");
    // an older daemon has no settings: the switch does nothing.
    rig.click(cx, "agent-tools");
    assert!(!rig.asked().iter().any(|c| matches!(c, Cmd::SetSettings(_))));

    rig.feed(cx, vec![UiEvent::DaemonSettings(tessel_proto::DaemonSettings { agent_tools: true, ..Default::default() })]);
    rig.click(cx, "agent-tools");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::SetSettings(p) if *p == serde_json::json!({"agent_tools": false}))));
}

// ---- terminals --------------------------------------------------------------------

fn term_info(id: u64, session: Option<SessionId>) -> tessel_proto::term::TermInfo {
    tessel_proto::term::TermInfo { id, title: String::new(), cwd: "/tmp".into(), cols: 80, rows: 24, session, command: vec![], exited: None }
}

#[gpui_kit::test]
fn a_terminal_pane_opens_a_shell_for_the_session_shows_it_and_sends_keys_as_the_program_wants_them(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "column", "children": [
        {"module": "conversation"}, {"id": "t", "module": "terminal"}
    ]}}}"#);
    // the pane asks the daemon for a shell in the session's folder.
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::TermOpen { pane, session: Some(s), .. } if pane == "t" && *s == sid(1))), "{asked:?}");

    rig.feed(cx, vec![UiEvent::TermOpened { pane: "t".into(), session: Some(sid(1)), info: term_info(7, Some(sid(1))) }]);
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::TermAttach(7))), "and attaches to it: {asked:?}");
    assert!(asked.iter().any(|c| matches!(c, Cmd::TermResize(7, ..))), "sized to its pane: {asked:?}");
    rig.feed(cx, vec![UiEvent::TermOutput(7, b"\x1b]0;my shell\x07$ ".to_vec()), UiEvent::TermUpdated(tessel_proto::term::TermInfo { title: "my shell".into(), ..term_info(7, Some(sid(1))) })]);
    assert!(rig.exists(cx, "terminal-7"));

    rig.click(cx, "terminal-7");
    rig.asked();
    rig.press(cx, "l");
    rig.press(cx, "ctrl-c");
    rig.press(cx, "up");
    let typed: Vec<u8> = rig.asked().into_iter().filter_map(|c| if let Cmd::TermInput(7, b) = c { Some(b) } else { None }).flatten().collect();
    assert_eq!(typed, b"l\x03\x1b[A");
    // ⌘ stays the window's: the palette still opens from a terminal.
    rig.press(cx, "cmd-k");
    assert!(rig.exists(cx, "palette"));
}

#[gpui_kit::test]
fn a_shell_already_running_for_the_session_is_shown_rather_than_starting_another(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.feed(cx, vec![UiEvent::Terms(vec![term_info(3, Some(sid(1))), term_info(4, Some(sid(2)))])]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "column", "children": [
        {"module": "conversation"}, {"id": "t", "module": "terminal"}
    ]}}}"#);
    let asked = rig.asked();
    assert!(!asked.iter().any(|c| matches!(c, Cmd::TermOpen { .. })), "{asked:?}");
    assert!(asked.iter().any(|c| matches!(c, Cmd::TermAttach(3))), "its own session's, not another's: {asked:?}");
}

// ---- git ----------------------------------------------------------------------------

fn commit(sha: &str, parents: &[&str], subject: &str) -> tessel_proto::Commit {
    tessel_proto::Commit { sha: sha.into(), parents: parents.iter().map(|p| (*p).to_owned()).collect(), refs: vec![], author: "Seb".into(), time_ms: 0, subject: subject.into() }
}

#[gpui_kit::test]
fn the_git_pane_asks_for_history_and_the_work_tree_stages_and_shows_a_commit(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "row", "children": [
        {"module": "conversation"}, {"module": "git"}
    ]}}}"#);
    let asked = rig.asked();
    let ask = |asked: &[Cmd], want: fn(&Request) -> bool| asked.iter().any(|c| matches!(c, Cmd::Ask { req, .. } if want(req)));
    assert!(ask(&asked, |r| matches!(r, Request::GitWorkTree { .. })), "{asked:?}");
    assert!(ask(&asked, |r| matches!(r, Request::GitLog { skip: 0, .. })), "{asked:?}");

    let tree = tessel_proto::WorkTree {
        is_repo: true,
        staged: vec![],
        unstaged: vec![tessel_proto::FileChange { path: "src/a.rs".into(), status: ChangeStatus::Modified, added: 2, removed: 1, binary: false }],
        undo: None,
    };
    let log = tessel_proto::GitLog { commits: vec![commit("b2", &["a1"], "second"), commit("a1", &[], "first")], more: false };
    rig.feed(cx, vec![
        UiEvent::Answer { tag: "git.tree", result: Ok(Response::WorkTree { tree }) },
        UiEvent::Answer { tag: "git.log", result: Ok(Response::GitLog { log }) },
    ]);
    assert!(rig.exists(cx, "commit-b2") && rig.exists(cx, "commit-a1"));

    // the button shows under the pointer.
    cx.update_window(rig.window, |_, window, cx| window.hover("git-unstaged-src/a.rs", cx)).unwrap();
    rig.settle(cx);
    rig.click(cx, "git-unstaged-src/a.rs-act");
    assert!(ask(&rig.asked(), |r| matches!(r, Request::GitStage { paths, .. } if paths == &["src/a.rs".to_owned()])));
    // a file of the work tree opens its own diff; lines picked in it are staged alone.
    rig.click(cx, "git-unstaged-src/a.rs");
    assert!(ask(&rig.asked(), |r| matches!(r, Request::GitDiff { path, index: Some(false), .. } if path == "src/a.rs")));
    rig.shell.update(cx, |s, cx| s.git_pick_lines_for_test(3, 4, cx));
    assert!(ask(&rig.asked(), |r| matches!(r, Request::GitStageLines { path, start: 3, end: 4, unstage: false, old: false, .. } if path == "src/a.rs")));
    rig.click(cx, "commit-a1");
    assert!(ask(&rig.asked(), |r| matches!(r, Request::GitShow { sha, .. } if sha == "a1")));
    // nothing staged: committing says what's missing instead of asking.
    rig.click(cx, "git-commit");
    assert!(!ask(&rig.asked(), |r| matches!(r, Request::GitCommit { .. })));
}

#[gpui_kit::test]
fn an_image_is_shown_and_markdown_is_read_as_a_document(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle.clone(), vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "row", "children": [
        {"module": "conversation"}, {"module": "files"}
    ]}}}"#);
    rig.feed(cx, vec![UiEvent::Dir { session: idle.id, path: String::new(), entries: vec![DirEntry { name: "logo.png".into(), is_dir: false, size: 70 }, DirEntry { name: "README.md".into(), is_dir: false, size: 22 }] }]);
    rig.feed(cx, vec![UiEvent::File { session: idle.id, file: FileContent { path: "logo.png".into(), text: None, binary: true, truncated: false, size: 70 } }]);
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Ask { tag: "file.preview", .. })), "a picture is asked for");
    let png = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
    rig.feed(cx, vec![UiEvent::Answer { tag: "file.preview", result: Ok(Response::Preview { mime: "image/png".into(), data: png.into(), note: None }) }]);
    assert!(rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().is_some_and(|f| f.preview.is_some() && f.note.is_none())));

    rig.feed(cx, vec![UiEvent::File { session: idle.id, file: FileContent { path: "README.md".into(), text: Some("# Hello\n\n- [ ] a task\n".into()), binary: false, truncated: false, size: 22 } }]);
    let state = rig.shell.read_with(cx, |s, _| s.ws.file.as_ref().map(|f| (f.path.clone(), f.rendered, f.preview.is_some(), f.editor.is_some())));
    assert!(rig.exists(cx, "file-document"), "read as a document: {state:?}");
    // ticking a to-do changes the source and saves it.
    rig.asked();
    let at = "# Hello\n\n- [ ] a task\n".find("[ ]").unwrap();
    let shell = rig.shell.clone();
    cx.update_window(rig.window, |_, window, cx| shell.update(cx, |s, cx| s.toggle_task(at..at + 3, true, window, cx))).unwrap();
    rig.settle(cx);
    assert!(
        rig.asked().iter().any(|c| matches!(c, Cmd::Write { path, content, .. } if path == "README.md" && content.contains("- [x] a task"))),
        "saved, ticked"
    );
    rig.click(cx, "file-render");
    assert!(!rig.exists(cx, "file-document") && rig.exists(cx, "file-save"), "and edited as source");
}

#[gpui_kit::test]
fn a_terminal_takes_composed_text_and_gives_its_program_the_mouse_when_asked(cx: &mut TestAppContext) {
    use gpui_kit::EntityInputHandler as _;
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "column", "children": [
        {"module": "conversation"}, {"id": "t", "module": "terminal"}
    ]}}}"#);
    rig.feed(cx, vec![UiEvent::TermOpened { pane: "t".into(), session: Some(sid(1)), info: term_info(7, Some(sid(1))) }]);
    rig.click(cx, "terminal-7");
    rig.asked();
    let typed = |rig: &mut Rig| -> Vec<u8> { rig.asked().into_iter().filter_map(|c| if let Cmd::TermInput(7, b) = c { Some(b) } else { None }).flatten().collect() };

    // a dead key: the accent waits, the letter settles it.
    let ime = rig.shell.read_with(cx, |s, _| s.term_ime_for_test(7));
    cx.update_window(rig.window, |_, window, cx| ime.update(cx, |i, cx| i.replace_and_mark_text_in_range(None, "´", None, window, cx))).unwrap();
    rig.settle(cx);
    assert!(typed(&mut rig).is_empty(), "nothing is sent while composing");
    cx.update_window(rig.window, |_, window, cx| ime.update(cx, |i, cx| i.replace_text_in_range(None, "é", window, cx))).unwrap();
    rig.settle(cx);
    assert_eq!(typed(&mut rig), "é".as_bytes());

    // the program turns on mouse reporting: a click is its, not a selection.
    rig.feed(cx, vec![UiEvent::TermOutput(7, b"\x1b[?1000h\x1b[?1006h".to_vec())]);
    rig.click(cx, "terminal-7");
    let sent = typed(&mut rig);
    assert!(sent.starts_with(b"\x1b[<0;"), "{:?}", String::from_utf8_lossy(&sent));
}

#[gpui_kit::test]
fn the_palette_finds_a_file_by_a_bit_of_its_name_and_opens_it(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "row", "children": [
        {"module": "conversation"}, {"module": "files"}
    ]}}}"#);
    rig.asked();
    rig.press(cx, "cmd-k");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Ask { req: Request::FsIndex { .. }, .. })), "the files are asked for");
    let paths = vec!["Cargo.toml".to_owned(), "crates/app/src/main.rs".to_owned(), "crates/app/src/maintenance.rs".to_owned(), "docs/readme.md".to_owned()];
    rig.feed(cx, vec![UiEvent::Answer { tag: "palette.files", result: Ok(Response::Paths { paths, more: false }) }]);
    rig.typed(cx, "main.rs");
    rig.press(cx, "enter");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Read { path, .. } if path == "crates/app/src/main.rs")), "the best match opens");
}

#[gpui_kit::test]
fn opening_and_closing_a_row_s_menu_doesn_t_fold_or_open_what_s_under_it(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    let folded = |rig: &mut Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.model.collapsed.len());
    let project = rig.shell.read_with(cx, |s, _| crate::model::project_name(&s.model.sessions[0]));
    let more = format!("project-{project}-more");
    let before = folded(&mut rig, cx);
    for _ in 0..2 {
        let row = ElementId::from(SharedString::from(format!("project-{project}")));
        cx.update_window(rig.window, |_, window, cx| window.hover(row, cx)).unwrap();
        rig.settle(cx);
        rig.click(cx, ElementId::from(SharedString::from(more.clone())));
    }
    assert_eq!(folded(&mut rig, cx), before, "the project stays as it was");

    rig.asked();
    let sid2 = sid(2).to_string();
    cx.update_window(rig.window, |_, window, cx| window.hover(ElementId::from(sid(2).0), cx)).unwrap();
    rig.settle(cx);
    rig.click(cx, ElementId::from(SharedString::from(format!("session-{sid2}-more"))));
    assert!(!rig.asked().iter().any(|c| matches!(c, Cmd::Open(_))), "opening a session's menu doesn't open the session");
}

#[gpui_kit::test]
fn all_sessions_always_has_a_way_back_and_says_when_there_are_none(cx: &mut TestAppContext) {
    let mut rig = Rig::new(cx);
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![])]);
    rig.press(cx, "shift-cmd-\\");
    assert!(rig.exists(cx, "overview-empty"), "an empty overview says so");
    rig.click(cx, "overview-close");
    assert!(!rig.shell.read_with(cx, |s, _| s.overview_for_test()), "and back is one click");
}

#[gpui_kit::test]
fn two_conversation_panes_hold_two_chats_and_a_new_one_mirrors(cx: &mut TestAppContext) {
    let a = session(1, "Fix login", RunState::Idle, None);
    let b = session(2, "Split the auth module", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, a.clone(), vec![]);
    rig.layout(cx, r#"{"root": {"type": "panes", "w": "full", "h": "full", "layout": {"split": "row", "children": [
        {"module": "conversation"}, {"module": "conversation", "id": "right"}
    ]}}}"#);
    let view = |rig: &Rig, cx: &mut TestAppContext, pane: &str| rig.shell.read_with(cx, |s, _| s.views.get(pane).map(|v| v.session));
    let open = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.model.open.as_ref().map(|o| o.id));
    // a new conversation pane shows the chat you're in: the same chat, nothing new to follow.
    assert_eq!(view(&rig, cx, "right"), Some(a.id));
    assert!(rig.exists(cx, "reply-bar"));
    assert!(!rig.asked().iter().any(|c| matches!(c, Cmd::Follow(_))), "a mirror is the chat already followed");

    // a chat opened on the left leaves the right one where it was.
    rig.shell.update(cx, |s, cx| s.open(b.id, cx));
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::Follow(extra) if *extra == vec![a.id])), "the right pane's chat is followed: {asked:?}");
    assert!(asked.iter().any(|c| matches!(c, Cmd::Open(Some(id)) if *id == b.id)));
    rig.feed(cx, vec![snapshot(b.clone(), vec![])]);
    assert_eq!((open(&rig, cx), view(&rig, cx, "right")), (Some(b.id), Some(a.id)));

    // what arrives for the right pane's chat goes there.
    let reply = Item { id: ItemId::from("t2"), parent: None, seq: 2, order: 2, created_ms: 0, body: ItemBody::Text { text: "done".into(), streaming: false } };
    rig.feed(cx, vec![UiEvent::Item { session: a.id, item: reply }]);
    assert_eq!(rig.shell.read_with(cx, |s, _| s.model.others.get(&a.id).map(|o| o.len())), Some(2));

    // half a message for b, then a click in the right pane: typing moves there with its chat, no fetching.
    cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.composer.update(cx, |c, cx| c.set_value("half a thought", window, cx)))).unwrap();
    rig.click(cx, "reply-bar");
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::Open(Some(id)) if *id == a.id)), "{asked:?}");
    assert!(asked.iter().any(|c| matches!(c, Cmd::Follow(extra) if *extra == vec![b.id])), "and the left pane's chat is followed now: {asked:?}");
    assert_eq!(open(&rig, cx), Some(a.id), "a is the focused chat, from what was on screen");
    assert_eq!(view(&rig, cx, "conversation"), Some(b.id), "the left pane keeps b");
    assert_eq!(rig.shell.read_with(cx, |s, cx| (s.drafts.get(&b.id).cloned(), s.composer.read(cx).value().to_string())), (Some("half a thought".into()), String::new()), "b's draft waits for b");
}

#[gpui_kit::test]
fn desktops_keep_their_own_chats_and_projects(cx: &mut TestAppContext) {
    let dir = layout_dir("desktops");
    let file = dir.join("ui.json");
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, a.clone(), vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let mut web = session(9, "Landing page", RunState::Idle, None);
    web.cwd = "/work/web".into();
    web.worktree = None;
    let mut all = rig.shell.read_with(cx, |s, _| s.model.sessions.clone());
    all.push(web.clone());
    rig.feed(cx, vec![UiEvent::Sessions(all)]);
    let exec = |rig: &mut Rig, cx: &mut TestAppContext, cmd: &str, args: serde_json::Value| {
        let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec(cmd, &args, window, cx))).unwrap();
        rig.settle(cx);
        r
    };
    let open = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.model.open.as_ref().map(|o| o.id).or(s.awaiting));

    // a second desktop: the layout in use becomes "Main", and the new one shows.
    let made = exec(&mut rig, cx, "desktop.new", serde_json::json!({"name": "Web", "scope": ["web"]})).unwrap();
    assert_eq!(made["number"], 2);
    let names = rig.shell.read_with(cx, |s, _| (s.ui.desktops.iter().map(|d| d.name.clone()).collect::<Vec<_>>(), s.ui.desktop));
    assert_eq!(names, (vec!["Main".to_owned(), "Web".to_owned()], 1));
    // it lists only its projects, and opens the latest chat among them.
    assert!(rig.exists(cx, web.id.0) && !rig.exists(cx, a.id.0), "the Web desktop lists web's chats only");
    assert_eq!(open(&rig, cx), Some(web.id));
    let asked = rig.asked();
    assert!(asked.iter().any(|c| matches!(c, Cmd::Follow(extra) if extra.contains(&a.id))), "Main's chat stays followed: {asked:?}");
    rig.feed(cx, vec![snapshot(web.clone(), vec![])]);

    // back to Main, by its name in the sidebar: its chat, from what was already loaded, and every project.
    assert!(rig.exists(cx, "desktop-1") && rig.exists(cx, "desktop-2"));
    rig.click(cx, "desktop-1");
    assert_eq!(rig.shell.read_with(cx, |s, _| s.ui.desktop), 0);
    assert_eq!(rig.shell.read_with(cx, |s, _| s.model.open.as_ref().map(|o| o.id)), Some(a.id), "no waiting: it was still on hand");
    assert!(rig.exists(cx, a.id.0) && rig.exists(cx, web.id.0));
    // the file remembers which chat Web had open.
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert_eq!(saved["desktops"][1]["open"], serde_json::json!(web.id.0.to_string()));

    // the landing page asks something while you're on Main: ⌘J takes you to its desktop.
    let mut asking = web.clone();
    asking.run_state = RunState::NeedsInput;
    asking.attention = Some(tessel_proto::Attention { reason: AttentionReason::NeedsInput, since_ms: 5, seq: 9 });
    rig.feed(cx, vec![UiEvent::Session(asking)]);
    rig.press(cx, "cmd-j");
    assert_eq!(rig.shell.read_with(cx, |s, _| (s.ui.desktop, s.model.open.as_ref().map(|o| o.id))), (1, Some(web.id)), "on Web, with the landing page open");

    // the previous desktop, then the next; then closing it goes back.
    exec(&mut rig, cx, "desktop.prev", serde_json::json!({})).unwrap();
    assert_eq!(rig.shell.read_with(cx, |s, _| s.ui.desktop), 0);
    exec(&mut rig, cx, "desktop.next", serde_json::json!({})).unwrap();
    assert_eq!(rig.shell.read_with(cx, |s, _| s.ui.desktop), 1);
    exec(&mut rig, cx, "desktop.close", serde_json::json!({})).unwrap();
    assert_eq!(rig.shell.read_with(cx, |s, _| (s.ui.desktops.len(), s.ui.desktop)), (1, 0));
    assert!(exec(&mut rig, cx, "desktop.close", serde_json::json!({})).is_err(), "the last one stays");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn a_chat_can_be_put_on_another_desktop_or_one_of_its_own(cx: &mut TestAppContext) {
    let dir = layout_dir("desktop-show");
    let file = dir.join("ui.json");
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, a.clone(), vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let exec = |rig: &mut Rig, cx: &mut TestAppContext, cmd: &str, args: serde_json::Value| {
        let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec(cmd, &args, window, cx))).unwrap();
        rig.settle(cx);
        r
    };
    let open = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| (s.ui.desktop, s.model.open.as_ref().map(|o| o.id).or(s.awaiting)));
    exec(&mut rig, cx, "desktop.new", serde_json::json!({"name": "Two"})).unwrap();

    // the overview shows every desktop, and a place to start another.
    rig.press(cx, "shift-cmd-\\");
    assert!(rig.exists(cx, "overview-desktop-1") && rig.exists(cx, "overview-desktop-2") && rig.exists(cx, "overview-desktop-new"));
    rig.press(cx, "escape");

    // "Split the auth module" onto Main, from Two: Main shows it when you go back.
    exec(&mut rig, cx, "desktop.show", serde_json::json!({"session": "Split the auth", "desktop": "Main"})).unwrap();
    assert_eq!(rig.shell.read_with(cx, |s, _| s.ui.desktop), 1, "you stay where you are");
    exec(&mut rig, cx, "desktop.use", serde_json::json!({"desktop": 1})).unwrap();
    assert_eq!(open(&rig, cx), (0, Some(sid(2))));

    // a desktop of its own: made, named after its project, showing it.
    let before = rig.shell.read_with(cx, |s, _| s.ui.desktops.len());
    exec(&mut rig, cx, "desktop.show", serde_json::json!({"session": "Write the docs", "desktop": "new"})).unwrap();
    let (n, name) = rig.shell.read_with(cx, |s, _| (s.ui.desktops.len(), s.ui.desktops[s.ui.desktop].name.clone()));
    assert_eq!((n, name.as_str()), (before + 1, "api"));
    assert_eq!(open(&rig, cx).1, Some(sid(3)));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn the_sidebar_stays_while_desktops_change_their_panes_and_a_swipe_switches(cx: &mut TestAppContext) {
    let dir = layout_dir("desktop-panes");
    let file = dir.join("ui.json");
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, a.clone(), vec![], Launch { ui: Some(file.clone()), ..Launch::default() });
    let exec = |rig: &mut Rig, cx: &mut TestAppContext, cmd: &str, args: serde_json::Value| {
        let r = cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec(cmd, &args, window, cx))).unwrap();
        rig.settle(cx);
        r
    };
    let panes = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.ui.panes().len());
    let showing = |rig: &Rig, cx: &mut TestAppContext| rig.shell.read_with(cx, |s, _| s.ui.desktop);
    let main_panes = panes(&rig, cx);
    exec(&mut rig, cx, "desktop.new", serde_json::json!({"name": "Two"})).unwrap();
    // splitting here changes this desktop's panes only, and never the sidebar.
    exec(&mut rig, cx, "pane.split", serde_json::json!({"pane": "conversation", "dir": "right"})).unwrap();
    assert_eq!(panes(&rig, cx), main_panes + 1);
    assert!(rig.exists(cx, a.id.0), "the sidebar's list, as it was");
    assert!(rig.shell.read_with(cx, |s, _| s.drawer("sidebar").is_some_and(|d| d.open)), "and the sidebar itself");
    exec(&mut rig, cx, "desktop.use", serde_json::json!({"desktop": 1})).unwrap();
    assert_eq!(panes(&rig, cx), main_panes, "Main's panes are its own");
    let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap();
    assert!(saved["desktops"][1]["layout"].is_object() && saved["desktops"][1].get("root").is_none(), "a desktop keeps panes, not a whole window");

    // two fingers to the left: the next desktop, once, however far the swipe coasts.
    let swipe = |rig: &mut Rig, cx: &mut TestAppContext, phase: TouchPhase, dx: f32| {
        let ev = ScrollEvent { delta: ScrollDelta::Pixels(point(px(dx), px(1.))), touch_phase: phase, ..ScrollEvent::default() };
        cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.sidebar_swipe(&ev, window, cx))).unwrap();
        rig.settle(cx);
    };
    swipe(&mut rig, cx, TouchPhase::Started, -20.);
    swipe(&mut rig, cx, TouchPhase::Moved, -50.);
    assert_eq!(showing(&rig, cx), 1);
    swipe(&mut rig, cx, TouchPhase::Moved, -90.);
    assert_eq!(showing(&rig, cx), 1, "one swipe, one desktop");
    // up and down is the list's, not a switch.
    swipe(&mut rig, cx, TouchPhase::Started, 0.);
    let ev = ScrollEvent { delta: ScrollDelta::Pixels(point(px(30.), px(120.))), touch_phase: TouchPhase::Moved, ..ScrollEvent::default() };
    cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.sidebar_swipe(&ev, window, cx))).unwrap();
    assert_eq!(showing(&rig, cx), 1);
    // and back to the right.
    swipe(&mut rig, cx, TouchPhase::Started, 30.);
    swipe(&mut rig, cx, TouchPhase::Moved, 40.);
    assert_eq!(showing(&rig, cx), 0);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn the_run_button_starts_the_dev_script_and_stops_it(cx: &mut TestAppContext) {
    let dir = layout_dir("run-button");
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, a.clone(), vec![], Launch { ui: Some(dir.join("ui.json")), ..Launch::default() });
    // nothing set up: the button asks an agent to set it up.
    rig.feed(cx, vec![UiEvent::Answer { tag: "scripts", result: Ok(Response::Scripts { scripts: tessel_proto::WorkspaceScripts::default(), port: 41000 }) }]);
    rig.click(cx, "run-default");
    match rig.asked().as_slice() {
        [Cmd::Create { cwd, worktree: false, prompt }] => assert!(*cwd == a.cwd && prompt.contains(".tessel/settings.toml") && prompt.contains("$TESSEL_PORT")),
        other => panic!("{other:?}"),
    }

    // a dev script: the button starts it, in a terminal pane of its own.
    let scripts = tessel_proto::WorkspaceScripts {
        actions: vec![tessel_proto::ScriptAction { id: "dev".into(), command: "npm run dev".into(), icon: None, default: true }],
        from: vec![".tessel/settings.toml".into()],
        ..Default::default()
    };
    rig.shell.update(cx, |s, _| {
        s.scripts.clear();
        s.scripts_tried.clear();
    });
    rig.settle(cx);
    rig.feed(cx, vec![UiEvent::Answer { tag: "scripts", result: Ok(Response::Scripts { scripts, port: 41000 }) }]);
    rig.click(cx, "run-default");
    let pane = match rig.asked().as_slice() {
        [Cmd::TermOpen { pane, session, command, .. }] => {
            assert_eq!((*session, command.as_slice()), (Some(a.id), &["npm run dev".to_owned()][..]));
            pane.clone()
        }
        other => panic!("{other:?}"),
    };
    assert!(rig.shell.read_with(cx, |s, _| s.ui.panes().iter().any(|p| p.id == pane && p.module == Some(crate::ui::Module::Terminal))), "a terminal pane to watch it in");

    // running: the same button stops it.
    let info = tessel_proto::term::TermInfo { id: 7, title: String::new(), cwd: a.cwd.clone(), cols: 100, rows: 30, session: Some(a.id), command: vec!["npm run dev".into()], exited: None };
    rig.feed(cx, vec![UiEvent::TermOpened { pane, session: Some(a.id), info }]);
    rig.click(cx, "run-default");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::TermClose(7))));
    std::fs::remove_dir_all(&dir).unwrap();
}

#[gpui_kit::test]
fn scripts_can_be_written_by_hand_for_the_team_or_this_mac(cx: &mut TestAppContext) {
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, a.clone(), vec![]);
    let scripts = tessel_proto::WorkspaceScripts {
        setup: Some("pnpm install".into()),
        actions: vec![tessel_proto::ScriptAction { id: "dev".into(), command: "pnpm dev".into(), icon: None, default: true }],
        copy: vec![".env*".into()],
        ..Default::default()
    };
    rig.feed(cx, vec![UiEvent::Answer { tag: "scripts", result: Ok(Response::Scripts { scripts, port: 41000 }) }]);
    cx.update_window(rig.window, |_, window, cx| rig.shell.update(cx, |s, cx| s.exec("app.settings", &serde_json::json!({"section": "scripts"}), window, cx))).unwrap().unwrap();
    rig.settle(cx);
    assert!(rig.exists(cx, "scripts-setup") && rig.exists(cx, "script-0-command"), "the page, filled in");
    rig.asked();
    rig.click(cx, "scripts-mode-alone");
    rig.click(cx, "scripts-save-local");
    match rig.asked().as_slice() {
        [Cmd::Ask { req: tessel_proto::Request::ScriptsSave { session, scripts, local: true }, .. }] => {
            assert_eq!(*session, a.id);
            assert_eq!((scripts.setup.as_deref(), scripts.concurrent), (Some("pnpm install"), false), "what the page says, one workspace at a time");
            assert_eq!(scripts.actions.iter().map(|a| (a.id.as_str(), a.command.as_str(), a.default)).collect::<Vec<_>>(), [("dev", "pnpm dev", true)]);
        }
        other => panic!("{other:?}"),
    }
    // the answer is what the workspace has now.
    let saved = tessel_proto::WorkspaceScripts { concurrent: false, ..Default::default() };
    rig.feed(cx, vec![UiEvent::Answer { tag: "scripts", result: Ok(Response::Scripts { scripts: saved, port: 41000 }) }]);
    assert_eq!(rig.shell.read_with(cx, |s, _| s.open_scripts().map(|(s, _)| s.concurrent)), Some(false));
}

#[gpui_kit::test]
fn the_browser_pane_shows_the_chats_page_and_a_pick_goes_in_your_message(cx: &mut TestAppContext) {
    use tessel_proto::BrowserAct;
    let dir = layout_dir("browser-pane");
    let a = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, a.clone(), vec![], Launch { ui: Some(dir.join("ui.json")), ..Launch::default() });
    let browser = |c: &Cmd| match c {
        Cmd::Browser { session, act } if *session == a.id => Some(act.clone()),
        _ => None,
    };

    // the agent uses the browser: a pane comes up, and the page is opened and watched.
    rig.feed(cx, vec![UiEvent::BrowserActivity { session: a.id, what: "clicked Sign in".into(), at: Some((40.0, 20.0)) }]);
    assert!(rig.shell.read_with(cx, |s, _| s.browser_visible()), "the agent's browsing is shown");
    let acts: Vec<BrowserAct> = rig.asked().iter().filter_map(browser).collect();
    assert!(acts.contains(&BrowserAct::Open { url: None }) && acts.contains(&BrowserAct::Watch { on: true }), "{acts:?}");
    assert!(rig.exists(cx, "browser-page"));

    // frames draw; the toolbar steers.
    rig.feed(cx, vec![UiEvent::BrowserFrame { session: a.id, bgra: vec![255; 4 * 4 * 4], width: 4, height: 4 }]);
    rig.click(cx, "browser-reload");
    assert!(rig.asked().iter().filter_map(browser).any(|a| a == BrowserAct::Reload));
    rig.click(cx, "browser-pick");
    assert!(rig.asked().iter().filter_map(browser).any(|a| a == BrowserAct::Pick { on: true }));

    // what you point at goes in your next message: its picture, and where it is.
    let picked = tessel_proto::Picked { url: "http://localhost:41000/".into(), selector: "#signin".into(), html: "<button id=\"signin\">Sign in</button>".into(), text: "Sign in".into(), x: 0.0, y: 0.0, width: 80.0, height: 30.0, image: "iVBORw0KGgo=".into() };
    rig.feed(cx, vec![UiEvent::BrowserPicked { session: a.id, picked }]);
    let (draft, attached) = rig.shell.read_with(cx, |s, cx| (s.composer.read(cx).value().to_string(), s.attachments.iter().map(|a| a.media_type.clone()).collect::<Vec<_>>()));
    assert!(draft.contains("#signin") && draft.contains("<button"), "{draft}");
    assert_eq!(attached, ["image/png"]);

    // no browser to drive: a way to get one.
    rig.feed(cx, vec![UiEvent::BrowserEngine(tessel_proto::BrowserEngine::default())]);
    assert!(rig.exists(cx, "browser-install"));
    rig.click(cx, "browser-install-go");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::Ask { tag: "browser.install", .. })));
    std::fs::remove_dir_all(&dir).unwrap();
}
