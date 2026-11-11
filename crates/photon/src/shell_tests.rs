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
use gpui_kit::{AnyWindowHandle, AppContext as _, ClipboardItem, ElementId, Entity, Image, ImageFormat, SharedString, TestAppContext, WindowOptions};
use photon_proto::*;
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
        cwd: "/work/api".into(),
        worktree: None,
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
        context_used: None,
        context_window: None,
        tasks: vec![], mcp: vec![], linked_dirs: vec![],
        cost_usd: 0.0,
        trace: vec![],
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
            out.push(c);
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
    rig.click(cx, "worktree-toggle");
    rig.click(cx, "new-prompt");
    rig.typed(cx, "fix the flaky test");
    rig.click(cx, "create");
    match rig.asked().as_slice() {
        [Cmd::Create { cwd, worktree: true, prompt }] => {
            assert!(!cwd.is_empty());
            assert_eq!(prompt, "fix the flaky test");
        }
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
    cx.update_window(rig.window, |_, window, cx| window.hover("toolbar-new", cx)).unwrap();
    rig.settle(cx);
    assert_eq!(order(&rig, cx), [1, 3, 2], "the session that needs you is on top");
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
    GitSummary { is_repo: true, branch: Some("photon/fix-login".into()), base: Some("origin/main".into()), ahead: 2, files, added, removed }
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
        base: "main".into(), head: "photon/fix-login".into(), checks: vec![check("build", CheckState::Passing), check("test", CheckState::Failing)],
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
        tool("w1", 3, "Write", serde_json::json!({"file_path": "/work/api/docs/notes.md"}), ToolStatus::Ok),
        tool("w2", 4, "Write", serde_json::json!({"file_path": "/work/api/never.md"}), ToolStatus::Failed),
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
    let model = |value: &str, id: &str, name: &str| ModelInfo { value: value.into(), id: id.into(), name: name.into(), description: String::new() };
    let models = vec![model("default", "claude-opus-5-5", "Default (recommended)"), model("opus", "claude-opus-5-5", "Opus 5.5"), model("sonnet", "claude-sonnet-5-5", "Sonnet 5.5")];
    rig.feed(cx, vec![UiEvent::Models(models), UiEvent::Sessions(vec![idle.clone()])]);
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
    let dir = std::env::temp_dir().join(format!("photon-ui-attach-{}", std::process::id()));
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
    cx.update_window(rig.window, |_, window, cx| window.hover("toolbar-new", cx)).unwrap();
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
    let dir = std::env::temp_dir().join(format!("photon-ui-{name}-{}", std::process::id()));
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
    assert!(rig.exists(cx, "toolbar-new") && !rig.exists(cx, sid(2).0), "the last layout that worked is still up");

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
fn asking_to_change_the_window_starts_an_agent_aside_that_knows_the_reference(cx: &mut TestAppContext) {
    let dir = layout_dir("customize");
    let launch = Launch { ui: Some(dir.join("ui.json")), ..Launch::default() };
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions_launched(cx, idle.clone(), vec![], launch);
    rig.press(cx, "shift-cmd-k");
    assert!(rig.exists(cx, "ask-bar"));
    rig.typed(cx, "put the changes under the conversation");
    rig.press(cx, "enter");
    match rig.asked().as_slice() {
        [Cmd::CreateAside { cwd, prompt, mode, .. }] => {
            assert_eq!(cwd, &dir.to_string_lossy());
            assert!(prompt.contains("ui/README.md") && prompt.contains("ui/check.txt") && prompt.ends_with("put the changes under the conversation"), "{prompt}");
            assert_eq!(mode, "acceptEdits", "it edits without stopping to ask");
        }
        other => panic!("{other:?}"),
    }
    // it works beside what is open, which stays open.
    let agent = session(9, "Changing this window", RunState::Working, None);
    rig.feed(cx, vec![UiEvent::Session(agent.clone()), UiEvent::Aside(agent.id)]);
    assert!(rig.shell.read_with(cx, |s, _| s.model.open.as_ref().map(|o| o.id) == Some(idle.id)));
    // the next ask goes to the same agent.
    rig.typed(cx, "and close files");
    rig.press(cx, "enter");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Send { session, text, .. }] if *session == agent.id && text == "and close files"));
    rig.press(cx, "escape");
    assert!(!rig.exists(cx, "ask-bar"));
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

    rig.click(cx, "palette-orchard");
    assert_eq!(read()["theme"]["base"], "orchard");
    assert_eq!(read()["theme"]["font"], "Inter", "what wasn't touched stays");
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
    std::fs::write(&file, format!(r#"{{"theme": {{"base": "tern"}}, "root": {}}}"#, &SIDEBAR_SHUT[9..SIDEBAR_SHUT.len() - 1])).unwrap();
    rig.settle(cx);
    rig.click(cx, "settings-tab-customize");
    rig.click(cx, "settings-reset-layout");
    assert!(read().get("root").is_none() && read()["theme"]["base"] == "tern");

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
    rig.feed(cx, vec![UiEvent::Connected, UiEvent::Sessions(vec![])]);
    rig.asked();
    rig.click(cx, "worktree-toggle");
    rig.click(cx, "in-place");
    rig.click(cx, "new-prompt");
    rig.typed(cx, "tidy the readme");
    rig.click(cx, "create");
    assert!(matches!(rig.asked().as_slice(), [Cmd::Create { worktree: false, prompt, .. }] if prompt == "tidy the readme"));
}

#[gpui_kit::test]
fn the_model_and_mode_pickers_open_above_the_message_box_and_pick(cx: &mut TestAppContext) {
    let mut idle = session(1, "Fix login", RunState::Idle, None);
    idle.model = Some("claude-sonnet-5-5".into());
    idle.permission_mode = Some("default".into());
    let model = |value: &str, id: &str, name: &str| ModelInfo { value: value.into(), id: id.into(), name: name.into(), description: "x".into() };
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.feed(cx, vec![UiEvent::Models(vec![model("opus", "claude-opus-5-5", "Opus 5.5"), model("sonnet", "claude-sonnet-5-5", "Sonnet 5.5"), model("claude-opus-4-7", "claude-opus-4-7", "Opus 4.7")])]);
    rig.click(cx, "model-menu");
    assert!(rig.exists(cx, "model-search"), "the picker opens with its search");
    let (menu, trigger) = cx.update_window(rig.window, |_, window, _| (window.find("model-list").bounds(), window.find("model-menu").bounds())).unwrap();
    assert!(menu.origin.y + menu.size.height <= trigger.origin.y + gpui_kit::px(1.), "it opens above the button: {menu:?} {trigger:?}");
    rig.asked();
    rig.click(cx, "model-opus");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetModel { model, .. }] if model == "opus"));
    assert!(!rig.exists(cx, "model-search"), "picking closes it");

    rig.click(cx, "mode-menu");
    rig.click(cx, "mode-plan");
    assert!(matches!(rig.asked().as_slice(), [Cmd::SetMode { mode, .. }] if mode == "plan"));
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

    let width = |rig: &Rig, cx: &mut TestAppContext, id: &'static str| cx.update_window(rig.window, |_, window, _| window.find(id).bounds().size.width).unwrap();
    rig.press(cx, "alt-cmd-i");
    // the split took the focus; clicking the conversation brings it back.
    rig.click(cx, "pane-conversation-header");
    let before = width(&rig, cx, "pane-conversation-header");
    rig.press(cx, "shift-cmd-enter");
    let maximized = width(&rig, cx, "pane-conversation-header");
    assert!(maximized > before, "maximized: {before:?} -> {maximized:?}");
    rig.press(cx, "shift-cmd-enter");
    let restored = width(&rig, cx, "pane-conversation-header");
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
    rig.feed(cx, vec![UiEvent::Call { call: 5, cmd: "theme.set".into(), args: serde_json::json!({"base": "tern"}) }]);
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
fn agents_can_be_kept_from_photons_tools_from_settings(cx: &mut TestAppContext) {
    let idle = session(1, "Fix login", RunState::Idle, None);
    let mut rig = Rig::with_sessions(cx, idle, vec![]);
    rig.press(cx, "cmd-,");
    rig.click(cx, "settings-tab-agents");
    // an older daemon has no settings: the switch does nothing.
    rig.click(cx, "agent-tools");
    assert!(!rig.asked().iter().any(|c| matches!(c, Cmd::SetSettings(_))));

    rig.feed(cx, vec![UiEvent::DaemonSettings(photon_proto::DaemonSettings { agent_tools: true })]);
    rig.click(cx, "agent-tools");
    assert!(rig.asked().iter().any(|c| matches!(c, Cmd::SetSettings(p) if *p == serde_json::json!({"agent_tools": false}))));
}

// ---- terminals --------------------------------------------------------------------

fn term_info(id: u64, session: Option<SessionId>) -> photon_proto::term::TermInfo {
    photon_proto::term::TermInfo { id, title: String::new(), cwd: "/tmp".into(), cols: 80, rows: 24, session, command: vec![], exited: None }
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
    rig.feed(cx, vec![UiEvent::TermOutput(7, b"\x1b]0;my shell\x07$ ".to_vec()), UiEvent::TermUpdated(photon_proto::term::TermInfo { title: "my shell".into(), ..term_info(7, Some(sid(1))) })]);
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
