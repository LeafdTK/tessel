//! the session state machine against real recorded sessions.

use photon_agent::claude::ClaudeNormalizer;
use photon_agent::recording::{self, Dir, RecLine};
use photon_proto::*;
use photond::reducer::{Effects, Input, Meta, STALL_MS, SessionState};
use serde_json::Value;
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "plain", "perm_allow", "perm_deny", "interrupt", "interrupt_text", "edit", "ask", "subagents", "kill9", "resume",
    "thinking", "model", "plan", "subagents_text",
];
const T0: Millis = 1_000_000;

fn load(name: &str) -> Vec<RecLine> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../photon-agent/tests/fixtures")
        .join(format!("{name}.jsonl"));
    recording::load(path).unwrap()
}

fn meta() -> Meta {
    Meta { id: SessionId::new(), goal: "test".into(), cwd: "/tmp/x".into(), worktree: None, parent: None, created_ms: T0, linked_dirs: vec![] }
}

/// a recording as the inputs the daemon would have fed the reducer, with their times.
fn inputs(recs: &[RecLine]) -> Vec<(Millis, Input)> {
    let mut n = ClaudeNormalizer::new();
    let mut out = Vec::new();
    let mut users = 0;
    for r in recs {
        let now = T0 + r.t;
        match r.dir {
            Dir::Out => {
                let evs = n.push_line(r.text().unwrap());
                if evs.is_empty() {
                    out.push((now, Input::Alive));
                }
                out.extend(evs.into_iter().map(|e| (now, Input::Agent(e))));
            }
            Dir::In => {
                let v: Value = serde_json::from_str(r.text().unwrap()).unwrap();
                match v["type"].as_str() {
                    Some("user") => {
                        users += 1;
                        let text = v["message"]["content"].as_str().unwrap().to_owned();
                        out.push((now, Input::UserSent { item_id: ItemId(format!("u-{users}")), text, attachments: vec![] }));
                    }
                    Some("control_response") => {
                        let resp = &v["response"];
                        let decision = match resp["response"]["behavior"].as_str() {
                            Some("allow") => PermissionDecision::Allow { updated_input: None },
                            Some("deny") => PermissionDecision::Deny { message: "no".into() },
                            _ => continue,
                        };
                        let request_id = RequestId(resp["request_id"].as_str().unwrap().to_owned());
                        out.push((now, Input::Answered { request_id, decision }));
                    }
                    _ => {}
                }
            }
            Dir::Meta => {
                if r.line.get("exit").is_some() {
                    out.push((now, Input::Agent(AgentEvent::Exited { code: r.line["exit"].as_i64().map(|c| c as i32) })));
                }
            }
        }
    }
    out
}

fn run(name: &str) -> (SessionState, Millis) {
    let mut s = SessionState::new(meta());
    s.apply(Input::Spawning, T0);
    let mut last = T0;
    for (now, input) in inputs(&load(name)) {
        s.apply(input, now);
        last = now;
    }
    (s, last)
}

fn tools(s: &SessionState) -> Vec<(String, ToolStatus)> {
    s.items()
        .iter()
        .filter_map(|i| match &i.body {
            ItemBody::Tool { tool, status, .. } => Some((tool.clone(), *status)),
            _ => None,
        })
        .collect()
}

fn assert_settled(s: &SessionState, now: Millis, ctx: &str) {
    assert!(!s.has_open_items(), "{ctx}: something is still open");
    assert!(s.pending().is_empty(), "{ctx}: a request is still pending");
    assert!(!s.turn_open(), "{ctx}: a turn is still open");
    for i in s.items() {
        match &i.body {
            ItemBody::Text { streaming, .. } | ItemBody::Thinking { streaming, .. } => assert!(!streaming, "{ctx}: {} still streaming", i.id),
            ItemBody::Tool { status, .. } => assert!(!status.is_open(), "{ctx}: tool {} still {status:?}", i.id),
            _ => {}
        }
    }
    let rs = s.run_state(now + 10 * STALL_MS);
    assert!(matches!(rs, RunState::Idle | RunState::Errored | RunState::Limited { .. }), "{ctx}: run state {rs:?}");
}

// ---- end states --------------------------------------------------------

#[test]
fn a_finished_turn_is_idle_and_wants_to_be_seen() {
    for name in ["plain", "perm_allow", "edit", "ask", "subagents", "resume"] {
        let (s, now) = run(name);
        assert_eq!(s.run_state(now), RunState::Idle, "{name}");
        assert_eq!(s.attention.as_ref().map(|a| a.reason), Some(AttentionReason::Finished), "{name}");
        // `plain` is a bare capture of the agent's output with no exit record;
        // the others were recorded through to the process exiting.
        assert_eq!(s.is_alive(), name == "plain", "{name}");
        assert!(s.cost_usd() > 0.0, "{name}");
        assert_settled(&s, now, name);
    }
}

#[test]
fn tools_end_in_the_right_state() {
    assert_eq!(tools(&run("perm_allow").0), [("Bash".into(), ToolStatus::Ok)]);
    assert_eq!(tools(&run("perm_deny").0), [("Bash".into(), ToolStatus::Denied)]);
    assert_eq!(tools(&run("edit").0), [("Write".into(), ToolStatus::Ok), ("Edit".into(), ToolStatus::Ok)]);
    assert_eq!(tools(&run("ask").0), [("AskUserQuestion".into(), ToolStatus::Ok)]);
    let sub = tools(&run("subagents").0);
    assert_eq!(sub.iter().filter(|(t, s)| t == "Agent" && *s == ToolStatus::Ok).count(), 2);
    assert_eq!(sub.iter().filter(|(t, s)| t == "Bash" && *s == ToolStatus::Ok).count(), 2);
}

#[test]
fn an_interrupt_is_not_an_error_and_does_not_nag() {
    for name in ["interrupt", "interrupt_text"] {
        let (s, now) = run(name);
        assert_eq!(s.run_state(now), RunState::Idle, "{name}");
        assert_eq!(s.attention, None, "{name}: you interrupted it, you already know");
        assert!(s.items().iter().any(|i| matches!(&i.body, ItemBody::Notice { level: NoticeLevel::Info, text } if text == "interrupted")), "{name}");
        assert_settled(&s, now, name);
    }
    // the tool that was running when you interrupted is closed, not left spinning.
    assert_eq!(tools(&run("interrupt").0), [("Bash".into(), ToolStatus::Failed)]);
}

#[test]
fn a_killed_agent_is_an_error_that_wants_you() {
    let (s, now) = run("kill9");
    assert_eq!(s.run_state(now), RunState::Errored);
    assert_eq!(s.attention.as_ref().map(|a| a.reason), Some(AttentionReason::Errored));
    assert_eq!(tools(&s), [("Bash".into(), ToolStatus::Aborted)]);
    assert!(s.items().iter().any(|i| matches!(&i.body, ItemBody::Notice { level: NoticeLevel::Error, .. })));
    assert!(s.agent_session_id.is_some(), "needed to resume it");
    assert_settled(&s, now, "kill9");
}

// ---- the property the whole design rests on ---------------------------------

/// cut any session off after any input and take the process away: nothing may
/// be left open, pending, or spinning. no forever-spinners, by construction.
#[test]
fn killing_the_agent_at_any_point_always_settles() {
    for name in FIXTURES {
        let all = inputs(&load(name));
        for cut in 0..=all.len() {
            let mut s = SessionState::new(meta());
            s.apply(Input::Spawning, T0);
            let mut now = T0;
            for (t, input) in &all[..cut] {
                s.apply(input.clone(), *t);
                now = *t;
            }
            s.apply(Input::Agent(AgentEvent::Exited { code: None }), now + 1);
            assert_settled(&s, now + 1, &format!("{name} cut at {cut}"));
            assert!(!s.is_alive());
        }
    }
}

/// same, for the daemon itself dying: rebuild from what was persisted.
#[test]
fn a_daemon_restart_at_any_point_always_settles() {
    for name in FIXTURES {
        let all = inputs(&load(name));
        for cut in 0..=all.len() {
            let mut s = SessionState::new(meta());
            s.apply(Input::Spawning, T0);
            let mut now = T0;
            for (t, input) in &all[..cut] {
                s.apply(input.clone(), *t);
                now = *t;
            }
            let mut back = SessionState::restore(s.persisted(), s.items().to_vec());
            back.apply(Input::Recovered, now + 1);
            assert_settled(&back, now + 1, &format!("{name} restored at {cut}"));
            assert!(back.items().len() >= s.items().len());
            // and an untouched, finished session comes back exactly as it was.
            if !s.turn_open() && !s.has_open_items() && s.pending().is_empty() {
                assert_eq!(back.items(), s.items(), "{name} at {cut}");
                assert_eq!(back.attention, s.attention, "{name} at {cut}");
            }
            // a turn cut short by the restart is an error you get told about.
            if s.turn_open() {
                assert_eq!(back.attention.as_ref().map(|a| a.reason), Some(AttentionReason::Errored), "{name} at {cut}");
            }
        }
    }
}

// ---- derived run state ---------------------------------------------------

/// while anything is waiting on the user the session says so, whatever else
/// happens. (two subagents: one blocked on a prompt while the other works.)
#[test]
fn blocked_stays_blocked_while_other_work_continues() {
    let mut s = SessionState::new(meta());
    let mut blocked_steps = 0;
    for (now, input) in inputs(&load("subagents")) {
        s.apply(input, now);
        if !s.pending().is_empty() {
            assert_eq!(s.run_state(now), RunState::NeedsPermission);
            assert_eq!(s.attention.as_ref().map(|a| a.reason), Some(AttentionReason::NeedsPermission));
            blocked_steps += 1;
        }
    }
    assert!(blocked_steps >= 2, "the recording has two prompts");
    // the recording answers each prompt within a millisecond, so no other work
    // lands while one is pending. prove that case directly.
    let mut s = SessionState::new(meta());
    let req = PermissionRequest {
        request_id: RequestId::from("r1"), tool_item: Some(ItemId::from("t1")), tool: "Bash".into(),
        input: serde_json::json!({"command": "rm -rf build"}), description: None, is_question: false, plan: None, suggestions: vec![],
    };
    s.apply(Input::Agent(AgentEvent::Phase { phase: AgentPhase::Running }), T0);
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: ItemId::from("t1"), parent: None, tool: "Bash".into(), input: req.input.clone() }), T0);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: req }), T0 + 1);
    assert_eq!(s.item(&ItemId::from("t1")).map(|i| matches!(i.body, ItemBody::Tool { status: ToolStatus::AwaitingPermission, .. })), Some(true));
    // another subagent starts a tool, streams text, reports progress...
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: ItemId::from("t2"), parent: None, tool: "Read".into(), input: serde_json::json!({"file_path": "/a/b.rs"}) }), T0 + 2);
    s.apply(Input::Agent(AgentEvent::Phase { phase: AgentPhase::Running }), T0 + 3);
    s.apply(Input::Agent(AgentEvent::ToolResult { item_id: ItemId::from("t2"), ok: true, output: "x".into() }), T0 + 4);
    assert_eq!(s.run_state(T0 + 5), RunState::NeedsPermission);
    assert_eq!(s.activity().unwrap().0, "wants to: running rm -rf build");
}

#[test]
fn a_question_is_needs_input_and_a_permission_outranks_it() {
    let mut s = SessionState::new(meta());
    let q = PermissionRequest { request_id: RequestId::from("q"), tool_item: None, tool: "AskUserQuestion".into(), input: Value::Null, description: None, is_question: true, plan: None, suggestions: vec![] };
    let p = PermissionRequest { request_id: RequestId::from("p"), tool_item: None, tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: q }), T0);
    assert_eq!(s.run_state(T0), RunState::NeedsInput);
    assert_eq!(s.attention.as_ref().unwrap().reason, AttentionReason::NeedsInput);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: p }), T0 + 500);
    assert_eq!(s.run_state(T0 + 500), RunState::NeedsPermission);
    let a = s.attention.clone().unwrap();
    assert_eq!((a.reason, a.since_ms), (AttentionReason::NeedsPermission, T0), "still the same wait: keeps its start time");
    s.apply(Input::Answered { request_id: RequestId::from("p"), decision: PermissionDecision::Deny { message: String::new() } }, T0 + 600);
    assert_eq!(s.run_state(T0 + 600), RunState::NeedsInput);
    s.apply(Input::Answered { request_id: RequestId::from("q"), decision: PermissionDecision::Allow { updated_input: None } }, T0 + 700);
    assert_eq!(s.attention, None, "nothing is waiting on you any more");
}

#[test]
fn a_long_tool_is_not_a_stall_but_silence_is() {
    let mut s = SessionState::new(meta());
    s.apply(Input::UserSent { item_id: ItemId::from("u1"), text: "go".into(), attachments: vec![] }, T0);
    assert_eq!(s.run_state(T0 + STALL_MS - 1), RunState::Working);
    assert_eq!(s.run_state(T0 + STALL_MS + 1), RunState::Stalled, "turn open, nothing in flight, silent");
    // any sign of life resets it.
    s.apply(Input::Alive, T0 + STALL_MS + 2);
    assert_eq!(s.run_state(T0 + STALL_MS + 3), RunState::Working);
    // a ten minute build is not a stall.
    let t = T0 + STALL_MS + 10;
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: ItemId::from("t"), parent: None, tool: "Bash".into(), input: serde_json::json!({"command": "cargo build"}) }), t);
    assert_eq!(s.run_state(t + 10 * 60_000), RunState::Working);
    let (line, since) = s.activity().unwrap();
    assert_eq!((line.as_str(), since), ("running cargo build", t));
    // once it returns and the agent goes quiet again, the clock restarts from the result.
    s.apply(Input::Agent(AgentEvent::ToolResult { item_id: ItemId::from("t"), ok: true, output: String::new() }), t + 600_000);
    assert_eq!(s.run_state(t + 600_000 + STALL_MS - 1), RunState::Working);
    assert_eq!(s.run_state(t + 600_000 + STALL_MS + 1), RunState::Stalled);
}

#[test]
fn a_turn_can_start_by_itself_and_still_reports_when_done() {
    // a background task finishing wakes the agent with nobody asking.
    let mut s = SessionState::new(meta());
    s.apply(Input::Agent(AgentEvent::TaskStarted { task_id: "bg".into(), tool_item: None, description: "run the dev server".into(), background: true }), T0);
    assert_eq!(s.run_state(T0), RunState::Idle);
    assert_eq!(s.summary(T0).background_tasks, 1);
    assert_eq!(s.activity().unwrap().0, "background: run the dev server");
    s.apply(Input::Agent(AgentEvent::Phase { phase: AgentPhase::Running }), T0 + 5_000);
    assert_eq!(s.run_state(T0 + 5_000), RunState::Working);
    s.apply(Input::Agent(AgentEvent::TurnFinished { outcome: TurnOutcome::Completed, cost_usd: Some(0.5), duration_ms: None }), T0 + 6_000);
    assert_eq!(s.attention.as_ref().unwrap().reason, AttentionReason::Finished);
}

#[test]
fn usage_limit_shows_as_limited_until_it_clears() {
    let mut s = SessionState::new(meta());
    s.apply(Input::Agent(AgentEvent::RateLimit { status: "rejected".into(), resets_at: Some(42), utilization: Some(1.0) }), T0);
    assert_eq!(s.run_state(T0), RunState::Limited { reset_at: Some(42) });
    s.apply(Input::Agent(AgentEvent::RateLimit { status: "allowed".into(), resets_at: None, utilization: Some(0.1) }), T0 + 1);
    assert_eq!(s.run_state(T0 + 1), RunState::Idle);
}

// ---- attention ------------------------------------------------------------

#[test]
fn finished_stays_flagged_until_you_have_seen_it() {
    let (mut s, now) = run("plain");
    let raised = s.attention.clone().unwrap();
    assert_eq!(raised.reason, AttentionReason::Finished);
    // twenty minutes later it is still flagged, from the moment it finished.
    assert_eq!(s.summary(now + 20 * 60_000).attention, Some(raised.clone()));
    // looking at an older point in the session does not clear it.
    s.apply(Input::MarkSeen { seq: raised.seq - 1 }, now + 1);
    assert_eq!(s.attention, Some(raised.clone()));
    let fx = s.apply(Input::MarkSeen { seq: s.seq }, now + 2);
    assert_eq!(s.attention, None);
    assert!(fx.summary_changed);
}

#[test]
fn looking_does_not_dismiss_a_pending_permission() {
    let mut s = SessionState::new(meta());
    let p = PermissionRequest { request_id: RequestId::from("p"), tool_item: None, tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: p }), T0);
    s.apply(Input::MarkSeen { seq: u64::MAX }, T0 + 1);
    assert_eq!(s.attention.as_ref().unwrap().reason, AttentionReason::NeedsPermission);
    assert_eq!(s.run_state(T0 + 1), RunState::NeedsPermission);
}

#[test]
fn sending_a_message_clears_finished_and_answering_is_idempotent() {
    let (mut s, now) = run("perm_allow");
    assert!(s.attention.is_some());
    s.apply(Input::UserSent { item_id: ItemId::from("again"), text: "more".into(), attachments: vec![] }, now + 1);
    assert_eq!(s.attention, None);
    assert_eq!(s.run_state(now + 1), RunState::Working);

    let mut s = SessionState::new(meta());
    let p = PermissionRequest { request_id: RequestId::from("p"), tool_item: None, tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: p.clone() }), T0);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: p }), T0);
    assert_eq!(s.pending().len(), 1, "the same request twice is one request");
    let answer = Input::Answered { request_id: RequestId::from("p"), decision: PermissionDecision::Allow { updated_input: None } };
    assert!(s.apply(answer.clone(), T0 + 1).pending_changed);
    let seq = s.seq;
    let again = s.apply(answer, T0 + 2);
    assert!(!again.pending_changed && s.seq == seq, "answering twice changes nothing");
}

// ---- transcript -------------------------------------------------------------

#[test]
fn our_own_message_is_shown_once() {
    let (s, _) = run("perm_allow");
    let users: Vec<_> = s.items().iter().filter(|i| matches!(i.body, ItemBody::User { .. })).collect();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].id, ItemId::from("u-1"), "shown the moment it was sent, not when the agent echoed it");
    // a session we only have the agent's side of still gets the message.
    let (s, _) = run("plain");
    assert_eq!(s.items().iter().filter(|i| matches!(i.body, ItemBody::User { .. })).count(), 1);
}

#[test]
fn streaming_text_adds_up_and_offsets_are_contiguous() {
    for name in ["plain", "interrupt_text", "perm_deny"] {
        let mut s = SessionState::new(meta());
        let mut streamed: std::collections::HashMap<ItemId, String> = Default::default();
        for (now, input) in inputs(&load(name)) {
            let fx: Effects = s.apply(input, now);
            for d in fx.deltas {
                let buf = streamed.entry(d.item_id.clone()).or_default();
                assert_eq!(d.offset as usize, buf.len(), "{name}: gap or overlap in deltas for {}", d.item_id);
                buf.push_str(&d.text);
            }
        }
        assert!(!streamed.is_empty(), "{name}");
        for (id, text) in streamed {
            match &s.item(&id).unwrap().body {
                ItemBody::Text { text: t, streaming } | ItemBody::Thinking { text: t, streaming } => {
                    assert!(!streaming);
                    assert!(t.starts_with(&text) || text.starts_with(t.as_str()), "{name}: {id}");
                }
                other => panic!("{name}: delta for a non-text item {other:?}"),
            }
        }
    }
}

#[test]
fn seq_only_goes_up_and_items_keep_their_place() {
    for name in FIXTURES {
        let mut s = SessionState::new(meta());
        let mut last = 0;
        for (now, input) in inputs(&load(name)) {
            let fx = s.apply(input, now);
            assert!(s.seq >= last, "{name}");
            for item in &fx.upserts {
                assert!(item.seq <= s.seq && item.order <= item.seq, "{name}");
            }
            if !fx.upserts.is_empty() || fx.pending_changed {
                assert!(s.seq > last, "{name}: a persisted change must bump seq");
                assert!(fx.summary_changed, "{name}");
            }
            last = s.seq;
        }
        let orders: Vec<_> = s.items().iter().map(|i| i.order).collect();
        assert!(orders.windows(2).all(|w| w[0] < w[1]), "{name}: transcript order must be strictly increasing");
    }
}

#[test]
fn paging_and_snapshots() {
    let (s, now) = run("subagents");
    let n = s.items().len();
    assert!(n > 6);
    let snap = s.snapshot(3, now);
    assert_eq!(snap.items.len(), 3);
    assert!(snap.has_more);
    assert_eq!(snap.items.last().unwrap().id, s.items().last().unwrap().id);
    let (older, more) = s.items_before(snap.items[0].order, 2);
    assert_eq!(older.len(), 2);
    assert_eq!(older[1].id, s.items()[n - 4].id);
    assert_eq!(more, n > 5);
    let (all, more) = s.items_before(u64::MAX, 10_000);
    assert_eq!((all.len(), more), (n, false));
    assert!(!s.snapshot(10_000, now).has_more);
}

#[test]
fn restore_gives_back_the_same_session() {
    let (s, now) = run("edit");
    let mut back = SessionState::restore(s.persisted(), s.items().to_vec());
    back.apply(Input::Recovered, now);
    assert_eq!(back.items(), s.items());
    assert_eq!(back.summary(now).attention, s.summary(now).attention);
    assert_eq!(back.cost_usd(), s.cost_usd());
    assert_eq!(back.agent_session_id, s.agent_session_id);
    assert_eq!(back.run_state(now), RunState::Idle);
}

#[test]
fn cost_accumulates_across_agent_processes() {
    let mut s = SessionState::new(meta());
    let done = |c| Input::Agent(AgentEvent::TurnFinished { outcome: TurnOutcome::Completed, cost_usd: Some(c), duration_ms: None });
    s.apply(done(0.10), T0);
    s.apply(done(0.25), T0 + 1); // the process reports a running total
    assert!((s.cost_usd() - 0.25).abs() < 1e-9);
    s.apply(Input::Agent(AgentEvent::Exited { code: Some(0) }), T0 + 2);
    s.apply(done(0.05), T0 + 3); // a new process starts from zero
    assert!((s.cost_usd() - 0.30).abs() < 1e-9);
}

#[test]
fn huge_tool_output_is_capped_on_the_item() {
    let mut s = SessionState::new(meta());
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: ItemId::from("t"), parent: None, tool: "Bash".into(), input: Value::Null }), T0);
    s.apply(Input::Agent(AgentEvent::ToolResult { item_id: ItemId::from("t"), ok: true, output: "é".repeat(100_000) }), T0 + 1);
    let ItemBody::Tool { output: Some(o), .. } = &s.item(&ItemId::from("t")).unwrap().body else { panic!() };
    assert!(o.len() < photond::reducer::MAX_OUTPUT + 64);
    assert!(o.contains("more bytes not shown"));
}

/// regression: a session blocked on a permission prompt when its process (or
/// the daemon) died used to come back with no flag at all, because the stale
/// "needs permission" flag outranked the error and was then cleared itself.
#[test]
fn dying_while_blocked_on_a_prompt_still_flags_the_error() {
    let mut s = SessionState::new(meta());
    let p = PermissionRequest { request_id: RequestId::from("p"), tool_item: None, tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    s.apply(Input::UserSent { item_id: ItemId::from("u"), text: "go".into(), attachments: vec![] }, T0);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: p }), T0 + 1);
    assert_eq!(s.attention.as_ref().unwrap().reason, AttentionReason::NeedsPermission);
    let fx = s.apply(Input::Agent(AgentEvent::Exited { code: Some(137) }), T0 + 2);
    assert!(fx.pending_changed);
    assert_eq!(s.attention.as_ref().map(|a| (a.reason, a.since_ms)), Some((AttentionReason::Errored, T0 + 2)));
    assert_eq!(s.run_state(T0 + 2), RunState::Errored);
}

/// a tool that waited on permission is timed from when it was allowed to run,
/// not from when the agent asked. (the app once showed "took 14372s" for a
/// one second command that had sat on a prompt for four hours.)
#[test]
fn a_tool_is_timed_from_when_it_started_running_not_when_it_was_requested() {
    let mut s = SessionState::new(meta());
    let t = ItemId::from("t");
    let req = PermissionRequest { request_id: RequestId::from("p"), tool_item: Some(t.clone()), tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    let times = |s: &SessionState| match &s.item(&t).unwrap().body {
        ItemBody::Tool { started_ms, finished_ms, .. } => (*started_ms, *finished_ms),
        _ => unreachable!(),
    };
    // the order claude really uses: the call, then the prompt for it.
    s.apply(Input::Agent(AgentEvent::Phase { phase: AgentPhase::Running }), T0);
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: t.clone(), parent: None, tool: "Bash".into(), input: Value::Null }), T0);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: req }), T0 + 5);
    assert_eq!(times(&s), (None, None), "waiting on you is not running");

    let four_hours = 4 * 3_600_000;
    s.apply(Input::Answered { request_id: RequestId::from("p"), decision: PermissionDecision::Allow { updated_input: None } }, T0 + four_hours);
    assert_eq!(times(&s), (Some(T0 + four_hours), None));
    assert_eq!(s.activity().unwrap().1, T0 + four_hours, "and the sidebar's elapsed time counts from then too");

    s.apply(Input::Agent(AgentEvent::ToolResult { item_id: t.clone(), ok: true, output: String::new() }), T0 + four_hours + 1_200);
    assert_eq!(times(&s), (Some(T0 + four_hours), Some(T0 + four_hours + 1_200)), "it ran for 1.2s");

    // with no prompt, it is running from the moment it is called.
    let u = ItemId::from("u");
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: u.clone(), parent: None, tool: "Read".into(), input: Value::Null }), T0 + 9);
    assert!(matches!(&s.item(&u).unwrap().body, ItemBody::Tool { started_ms: Some(at), .. } if *at == T0 + 9));

    // a denied call never ran at all.
    let d = ItemId::from("d");
    let req = PermissionRequest { request_id: RequestId::from("q"), tool_item: Some(d.clone()), tool: "Bash".into(), input: Value::Null, description: None, is_question: false, plan: None, suggestions: vec![] };
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: d.clone(), parent: None, tool: "Bash".into(), input: Value::Null }), T0 + 10);
    s.apply(Input::Agent(AgentEvent::PermissionRequested { request: req }), T0 + 11);
    s.apply(Input::Answered { request_id: RequestId::from("q"), decision: PermissionDecision::Deny { message: String::new() } }, T0 + 12);
    assert!(matches!(&s.item(&d).unwrap().body, ItemBody::Tool { status: ToolStatus::Denied, started_ms: None, .. }));
}

/// the same thing, on a real recording rather than hand-built events.
#[test]
fn the_recorded_permission_flow_times_the_tool_from_the_approval() {
    let mut s = SessionState::new(meta());
    let mut approved_at = None;
    for (now, input) in inputs(&load("perm_allow")) {
        if matches!(input, Input::Answered { .. }) {
            approved_at = Some(now);
        }
        s.apply(input, now);
    }
    let approved_at = approved_at.expect("the recording has an approval");
    let tool = s.items().iter().find(|i| matches!(i.body, ItemBody::Tool { .. })).unwrap();
    let ItemBody::Tool { started_ms, finished_ms, .. } = &tool.body else { unreachable!() };
    assert!(tool.created_ms < approved_at, "the call was made before it was approved");
    assert_eq!(*started_ms, Some(approved_at));
    assert!(finished_ms.unwrap() >= approved_at);
}

// ---- the trace ---------------------------------------------------------------

#[test]
fn the_trace_shows_output_where_it_happened_and_goes_flat_when_the_agent_does() {
    use photon_proto::{TRACE_BUCKET_MS as B, TRACE_BUCKETS as N};
    let mut s = SessionState::new(meta());
    assert!(s.trace(T0).is_empty(), "a silent session has no trace at all");

    let say = |s: &mut SessionState, text: &str, at: Millis| {
        s.apply(Input::Agent(AgentEvent::Delta { item_id: ItemId::from("x"), text: text.into() }), at);
    };
    say(&mut s, "0123456789", T0); // 10 bytes now
    say(&mut s, "abcde", T0 + 500); // same two-second bucket
    let t = s.trace(T0 + 600);
    assert_eq!(t.len(), N);
    assert_eq!(t[N - 1], 15, "both land in the bucket that is filling");
    assert!(t[..N - 1].iter().all(|v| *v == 0));

    // three buckets later it says more.
    say(&mut s, "xyz", T0 + 3 * B + 10);
    let t = s.trace(T0 + 3 * B + 20);
    assert_eq!((t[N - 4], t[N - 3], t[N - 2], t[N - 1]), (15, 0, 0, 3), "the gap is visible as empty buckets");

    // time passing with no output slides everything left: the newest buckets are flat.
    let later = s.trace(T0 + 10 * B);
    assert_eq!(later[N - 11], 15);
    assert!(later[N - 7..].iter().all(|v| *v == 0), "a stuck agent reads as a flat tail: {later:?}");

    // after a full minute of silence there is nothing left to draw.
    assert!(s.trace(T0 + (N as u64 + 4) * B).is_empty());
    // and the summary carries it.
    assert_eq!(s.summary(T0 + 10 * B).trace, later);
}

#[test]
fn a_turn_of_tool_calls_with_no_text_still_shows_as_alive() {
    let mut s = SessionState::new(meta());
    s.apply(Input::Agent(AgentEvent::ToolCall { item_id: ItemId::from("t"), parent: None, tool: "Bash".into(), input: Value::Null }), T0);
    assert!(s.trace(T0).last().copied().unwrap_or(0) > 0);
    // proof of life that is not output (hook chatter) does not count.
    let mut quiet = SessionState::new(meta());
    quiet.apply(Input::Alive, T0);
    quiet.apply(Input::Agent(AgentEvent::Phase { phase: AgentPhase::Running }), T0);
    assert!(quiet.trace(T0).is_empty());
}

#[test]
fn the_trace_survives_a_long_gap_and_a_huge_burst_without_wrapping() {
    use photon_proto::TRACE_BUCKET_MS as B;
    let mut s = SessionState::new(meta());
    let say = |s: &mut SessionState, n: usize, at: Millis| {
        s.apply(Input::Agent(AgentEvent::Delta { item_id: ItemId::from("x"), text: "a".repeat(n) }), at);
    };
    say(&mut s, 10, T0);
    // an hour later: the old bucket must be gone, not shifted into nonsense.
    say(&mut s, 7, T0 + 1800 * B);
    let t = s.trace(T0 + 1800 * B);
    assert_eq!(t.iter().map(|v| u32::from(*v)).sum::<u32>(), 7);
    // more than fits in the wire type is clamped, not wrapped to a small number.
    say(&mut s, 200_000, T0 + 1800 * B + 1);
    assert_eq!(*s.trace(T0 + 1800 * B + 2).last().unwrap(), u16::MAX);
}

#[test]
fn recorded_sessions_have_traces_that_end_flat() {
    for name in ["interrupt_text", "subagents", "plain"] {
        let (s, last) = run(name);
        let t = s.trace(last);
        assert!(!t.is_empty(), "{name} produced output");
        assert!(s.trace(last + 61_000).is_empty(), "{name}: a minute after the last output the trace is gone");
    }
}

// ---- model, mode, context, plans, subagents ------------------------------

#[test]
fn the_summary_says_which_model_is_running_and_how_full_its_context_is() {
    let (s, now) = run("model");
    let sum = s.summary(now);
    assert_eq!(sum.model.as_deref(), Some("claude-sonnet-5-5"), "we asked for `sonnet`; this is its real name");
    let (used, window) = (sum.context_used.unwrap(), sum.context_window.unwrap());
    assert!(used > 1_000 && used < window, "{used} of {window}");
    assert_eq!(sum.permission_mode.as_deref(), Some("default"));
}

#[test]
fn a_plan_waits_for_approval_and_approving_it_leaves_plan_mode() {
    let mut s = SessionState::new(meta());
    s.apply(Input::Spawning, T0);
    let mut waited = false;
    let mut last = T0;
    for (now, input) in inputs(&load("plan")) {
        s.apply(input, now);
        last = now;
        if let Some(p) = s.pending().first()
            && p.plan.is_some()
        {
            waited = true;
            let sum = s.summary(now);
            assert_eq!(sum.permission_mode.as_deref(), Some("plan"));
            assert_eq!(sum.run_state, RunState::NeedsPermission, "it is blocked on you, like any other request");
            assert_eq!(sum.activity.as_deref(), Some("has a plan for you to review"));
            assert!(p.plan.as_deref().unwrap().contains("hello.txt"));
        }
    }
    assert!(waited, "the plan was never offered");
    assert_eq!(s.summary(last).permission_mode.as_deref(), Some("default"), "approved: back to work");
    assert_settled(&s, last, "plan");
}

#[test]
fn running_subagents_are_listed_and_point_at_where_they_were_started() {
    for name in ["subagents", "subagents_text"] {
        let mut s = SessionState::new(meta());
        s.apply(Input::Spawning, T0);
        let mut most = Vec::new();
        let mut last = T0;
        for (now, input) in inputs(&load(name)) {
            s.apply(input, now);
            last = now;
            let tasks = s.summary(now).tasks;
            if tasks.len() > most.len() {
                most = tasks;
            }
        }
        assert_eq!(most.len(), 2, "{name}: both subagents were running at once");
        for t in &most {
            assert!(!t.description.is_empty(), "{name}");
            let item = t.item.as_ref().expect("a subagent knows the call that started it");
            assert!(matches!(s.item(item).map(|i| &i.body), Some(ItemBody::Tool { tool, .. }) if tool == "Agent"), "{name}");
        }
        assert!(s.summary(last).tasks.is_empty(), "{name}: and none are left when the turn is over");
    }
}

#[test]
fn what_a_subagent_wrote_sits_under_it_not_in_the_main_conversation() {
    let (s, _) = run("subagents_text");
    let agents: Vec<&ItemId> = s
        .items()
        .iter()
        .filter(|i| matches!(&i.body, ItemBody::Tool { tool, .. } if tool == "Agent"))
        .map(|i| &i.id)
        .collect();
    assert_eq!(agents.len(), 2);
    for a in agents {
        let kinds: Vec<&str> = s
            .items()
            .iter()
            .filter(|i| i.parent.as_ref() == Some(a))
            .map(|i| match &i.body {
                ItemBody::Text { .. } => "text",
                ItemBody::Thinking { .. } => "thinking",
                ItemBody::Tool { .. } => "tool",
                _ => "other",
            })
            .collect();
        assert!(kinds.contains(&"tool"), "{kinds:?}");
        assert!(kinds.contains(&"text") || kinds.contains(&"thinking"), "the subagent's own words are kept: {kinds:?}");
    }
}

#[test]
fn a_model_or_mode_chosen_while_nothing_is_running_is_kept_for_the_next_start() {
    let mut s = SessionState::new(meta());
    assert!(s.apply(Input::SetModel { model: "opus".into() }, T0).summary_changed);
    assert!(s.apply(Input::SetMode { mode: "plan".into() }, T0).summary_changed);
    assert!(!s.apply(Input::SetMode { mode: "plan".into() }, T0).summary_changed, "choosing the same again is not a change");
    let restored = SessionState::restore(s.persisted(), vec![]);
    assert_eq!((restored.model.as_deref(), restored.permission_mode.as_deref()), (Some("opus"), Some("plan")));
}

#[test]
fn context_use_survives_a_restart_and_a_new_model_forgets_the_old_window() {
    let (s, now) = run("plain");
    let before = s.summary(now);
    assert!(before.context_used.is_some() && before.context_window.is_some());
    let mut restored = SessionState::restore(s.persisted(), s.items().to_vec());
    let after = restored.summary(now);
    assert_eq!((after.context_used, after.context_window), (before.context_used, before.context_window));
    restored.apply(Input::Agent(AgentEvent::ModelChanged { model: "another".into() }), now);
    assert_eq!(restored.summary(now).context_window, None, "we don't know the new model's size until it says");
}

#[test]
fn a_request_the_agent_refused_is_shown_not_swallowed() {
    let mut s = SessionState::new(meta());
    let fx = s.apply(Input::Agent(AgentEvent::RequestFailed { message: "Model 'x' not found".into() }), T0);
    assert!(matches!(&fx.upserts[..], [Item { body: ItemBody::Notice { level: NoticeLevel::Warn, text }, .. }] if text.contains("not found")));
    assert_eq!(s.attention, None, "worth saying, not worth interrupting you for");
}

#[test]
fn a_sent_message_remembers_what_was_attached() {
    let mut s = SessionState::new(meta());
    s.apply(Input::UserSent { item_id: ItemId::from("u"), text: "look".into(), attachments: vec!["shot.png".into()] }, T0);
    assert!(matches!(&s.items()[0].body, ItemBody::User { attachments, .. } if attachments == &["shot.png".to_owned()]));
}

#[test]
fn tool_servers_are_listed_from_the_start_and_updated_when_the_agent_says_more() {
    let server = |name: &str, status: &str| McpServer { name: name.into(), status: status.into(), scope: None };
    let mut s = SessionState::new(meta());
    let init = AgentEvent::Init {
        agent_session_id: "a".into(), model: None, cwd: None, permission_mode: None, agent_version: None,
        mcp_servers: vec![server("docs", "pending")],
    };
    s.apply(Input::Agent(init), T0);
    assert_eq!(s.summary(T0).mcp, vec![server("docs", "pending")]);
    let fx = s.apply(Input::Agent(AgentEvent::McpServers { servers: vec![server("docs", "connected")] }), T0);
    assert!(fx.summary_changed);
    assert_eq!(s.summary(T0).mcp[0].status, "connected");
    assert!(!s.apply(Input::Agent(AgentEvent::McpServers { servers: vec![server("docs", "connected")] }), T0).summary_changed);
}

#[test]
fn linked_folders_are_kept_once_each_and_survive_a_restart() {
    let mut s = SessionState::new(meta());
    assert!(s.apply(Input::LinkDir { path: "/work/other".into() }, T0).summary_changed);
    assert!(!s.apply(Input::LinkDir { path: "/work/other".into() }, T0).summary_changed);
    let restored = SessionState::restore(s.persisted(), vec![]);
    assert_eq!(restored.summary(T0).linked_dirs, vec!["/work/other".to_owned()]);
    assert!(s.apply(Input::UnlinkDir { path: "/work/other".into() }, T0).summary_changed);
    assert!(s.summary(T0).linked_dirs.is_empty());
}

#[test]
fn switching_model_between_turns_leaves_the_session_idle() {
    let (mut s, now) = run("plain");
    let mut n = ClaudeNormalizer::new();
    let lines = [
        r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to `sonnet (claude-sonnet-5-5)`</local-command-stdout>"},"parent_tool_use_id":null,"isReplay":true}"#,
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"m1","response":{"model":"claude-sonnet-5-5"}}}"#,
        r#"{"type":"system","subtype":"session_state_changed","state":"idle"}"#,
    ];
    let users = s.items().iter().filter(|i| matches!(i.body, ItemBody::User { .. })).count();
    for line in lines {
        for ev in n.push_line(line) {
            s.apply(Input::Agent(ev), now + 1);
        }
    }
    assert_eq!(s.run_state(now + 2), RunState::Idle, "nothing was asked of the agent");
    assert_eq!(s.summary(now + 2).model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(s.items().iter().filter(|i| matches!(i.body, ItemBody::User { .. })).count(), users, "and no message appeared in the transcript");
}
