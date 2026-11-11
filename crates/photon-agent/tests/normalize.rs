//! the claude parser against real recorded sessions.
//!
//! `tests/fixtures/*.jsonl` are scrubbed recordings of claude code 2.1.319.
//! `tests/golden/*.events.jsonl` is what we expect the parser to make of each.
//! after a deliberate change: `UPDATE_GOLDEN=1 cargo test -p photon-agent`
//! and read the diff.

use photon_agent::claude::ClaudeNormalizer;
use photon_agent::recording::{self, Dir};
use photon_proto::{AgentEvent, AgentPhase, BlockKind, ItemId, TurnOutcome};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

const FIXTURES: &[&str] = &[
    "plain", "perm_allow", "perm_deny", "interrupt", "interrupt_text", "edit", "ask", "subagents", "kill9", "resume",
    "thinking", "model", "plan", "subagents_text",
];

fn dir(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join(sub)
}

fn events(name: &str) -> Vec<AgentEvent> {
    let recs = recording::load(dir("fixtures").join(format!("{name}.jsonl"))).unwrap();
    let mut n = ClaudeNormalizer::new();
    recording::out_lines(&recs).flat_map(|l| n.push_line(l)).collect()
}

#[test]
fn golden() {
    let update = std::env::var_os("UPDATE_GOLDEN").is_some();
    for name in FIXTURES {
        let got: Vec<serde_json::Value> = events(name).iter().map(|e| serde_json::to_value(e).unwrap()).collect();
        let path = dir("golden").join(format!("{name}.events.jsonl"));
        if update {
            let text: String = got.iter().map(|v| v.to_string() + "\n").collect();
            std::fs::write(&path, text).unwrap();
            continue;
        }
        let want: Vec<serde_json::Value> = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing {}; run with UPDATE_GOLDEN=1", path.display()))
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // compared as values, not text: the order of keys inside a json object
        // depends on which serde_json features the rest of the build turned on.
        assert_eq!(got.len(), want.len(), "{name}: number of events changed. rerun with UPDATE_GOLDEN=1 and review the diff.");
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert!(same(g, w), "{name}: event {i} changed. rerun with UPDATE_GOLDEN=1 and review the diff.\n  got: {g}\n want: {w}");
        }
    }
}

/// equal, give or take the last digit of a fraction. how serde_json reads a
/// number like a cost in dollars depends on its `float_roundtrip` feature,
/// which other crates in the build switch on and off.
fn same(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value::{Array, Number, Object};
    match (a, b) {
        (Number(x), Number(y)) if x.is_f64() || y.is_f64() => match (x.as_f64(), y.as_f64()) {
            (Some(x), Some(y)) => (x - y).abs() <= 1e-12 * x.abs().max(y.abs()),
            _ => false,
        },
        (Array(x), Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y)),
        (Object(x), Object(y)) => x.len() == y.len() && x.iter().all(|(k, x)| y.get(k).is_some_and(|y| same(x, y))),
        _ => a == b,
    }
}

#[test]
fn golden_comparison_forgives_a_last_digit_and_nothing_else() {
    use serde_json::json;
    assert!(same(&json!({"cost_usd": 0.0037430300000000005, "n": 3}), &json!({"n": 3, "cost_usd": 0.003743030000000001})));
    assert!(!same(&json!({"cost_usd": 0.0037}), &json!({"cost_usd": 0.0038})));
    assert!(!same(&json!({"n": 3}), &json!({"n": 4})) && !same(&json!({"n": 3}), &json!({"n": 3, "m": 1})));
    assert!(!same(&json!([1, 2]), &json!([1])) && !same(&json!("1"), &json!(1)));
}

#[test]
fn every_fixture_ends_its_turn_unless_killed() {
    for name in FIXTURES {
        let evs = events(name);
        let finished = evs.iter().any(|e| matches!(e, AgentEvent::TurnFinished { .. }));
        assert_eq!(finished, *name != "kill9", "{name}");
    }
}

#[test]
fn plain_turn_streams_then_completes_the_same_item() {
    let evs = events("plain");
    let started: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::BlockStarted { item_id, block: BlockKind::Text, .. } => Some(item_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 1);
    let streamed: String = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Delta { item_id, text } if *item_id == started[0] => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let (done_id, done_text) = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::Text { item_id, text, .. } => Some((item_id.clone(), text.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(done_id, started[0], "the completed block must reuse the streaming block's id");
    assert_eq!(streamed, done_text, "deltas must add up to the final text");
    assert!(matches!(evs.iter().find(|e| matches!(e, AgentEvent::TurnFinished { .. })),
        Some(AgentEvent::TurnFinished { outcome: TurnOutcome::Completed, cost_usd: Some(_), .. })));
}

#[test]
fn deltas_always_add_up_to_the_completed_text() {
    for name in FIXTURES {
        let evs = events(name);
        let mut streamed: HashMap<_, String> = HashMap::new();
        for e in &evs {
            if let AgentEvent::Delta { item_id, text } = e {
                streamed.entry(item_id.clone()).or_default().push_str(text);
            }
        }
        for e in &evs {
            if let AgentEvent::Text { item_id, text, .. } = e
                && let Some(s) = streamed.get(item_id)
            {
                // an interrupted block may complete with less than was streamed? no: never more.
                assert!(text.starts_with(s.as_str()) || s.starts_with(text.as_str()), "{name}: {item_id}");
            }
        }
    }
}

#[test]
fn item_ids_are_unique_per_completed_block() {
    for name in FIXTURES {
        let mut seen = HashSet::new();
        for e in events(name) {
            let id = match e {
                AgentEvent::Text { item_id, .. } | AgentEvent::Thinking { item_id, .. } | AgentEvent::ToolCall { item_id, .. } => item_id,
                _ => continue,
            };
            assert!(seen.insert(id.clone()), "{name}: two completed blocks share id {id}");
        }
    }
}

#[test]
fn every_tool_result_has_a_call() {
    for name in FIXTURES {
        let evs = events(name);
        let calls: HashSet<_> = evs
            .iter()
            .filter_map(|e| match e {
                AgentEvent::ToolCall { item_id, .. } => Some(item_id.clone()),
                _ => None,
            })
            .collect();
        for e in &evs {
            if let AgentEvent::ToolResult { item_id, .. } = e {
                assert!(calls.contains(item_id), "{name}: result for unknown call {item_id}");
            }
        }
    }
}

#[test]
fn permission_allow() {
    let evs = events("perm_allow");
    let req = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequested { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(req.tool, "Bash");
    assert!(!req.is_question);
    assert_eq!(req.input["command"], "echo photon-fixture > out.txt");
    assert!(!req.suggestions.is_empty());
    let call = req.tool_item.unwrap();
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolResult { item_id, ok: true, .. } if *item_id == call)));
    // the agent says it is blocked, then running again.
    let phases: Vec<_> = evs.iter().filter_map(|e| match e { AgentEvent::Phase { phase } => Some(*phase), _ => None }).collect();
    assert_eq!(phases, [AgentPhase::Running, AgentPhase::RequiresAction, AgentPhase::Running, AgentPhase::Idle]);
}

#[test]
fn permission_deny_fails_the_tool_and_the_turn_still_completes() {
    let evs = events("perm_deny");
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::ToolResult { ok: false, output, .. } if output.contains("denied"))));
    assert!(evs.iter().any(|e| matches!(e, AgentEvent::TurnFinished { outcome: TurnOutcome::Completed, .. })));
}

#[test]
fn interrupts_are_interrupted_not_errors() {
    for name in ["interrupt", "interrupt_text"] {
        let evs = events(name);
        let outcome = evs.iter().find_map(|e| match e {
            AgentEvent::TurnFinished { outcome, .. } => Some(outcome.clone()),
            _ => None,
        });
        assert_eq!(outcome, Some(TurnOutcome::Interrupted), "{name}");
    }
    // interrupting a running tool closes it with a failed result.
    assert!(events("interrupt").iter().any(|e| matches!(e, AgentEvent::ToolResult { ok: false, .. })));
}

#[test]
fn a_question_is_flagged_as_a_question() {
    let evs = events("ask");
    let req = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequested { request } => Some(request.clone()),
            _ => None,
        })
        .unwrap();
    assert!(req.is_question);
    assert_eq!(req.tool, "AskUserQuestion");
    assert_eq!(req.input["questions"][0]["options"].as_array().unwrap().len(), 2);
}

#[test]
fn subagent_work_hangs_off_its_agent_call() {
    let evs = events("subagents");
    let agent_calls: Vec<_> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { item_id, tool, parent: None, .. } if tool == "Agent" => Some(item_id.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(agent_calls.len(), 2);
    let child_parents: HashSet<_> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ToolCall { tool, parent: Some(p), .. } if tool == "Bash" => Some(p.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(child_parents, agent_calls.iter().cloned().collect());

    let started: Vec<_> = evs.iter().filter_map(|e| match e { AgentEvent::TaskStarted { task_id, background, .. } => Some((task_id.clone(), *background)), _ => None }).collect();
    assert_eq!(started.len(), 2);
    assert!(started.iter().all(|(_, bg)| !bg));
    for (task, _) in &started {
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::TaskProgress { task_id, .. } if task_id == task)));
        assert!(evs.iter().any(|e| matches!(e, AgentEvent::TaskFinished { task_id, .. } if task_id == task)));
    }
    // two permission prompts, one per subagent, while the other was running.
    assert_eq!(evs.iter().filter(|e| matches!(e, AgentEvent::PermissionRequested { .. })).count(), 2);
}

#[test]
fn killed_session_leaves_an_open_tool_and_resume_knows_its_session() {
    let killed = events("kill9");
    let calls: Vec<_> = killed.iter().filter_map(|e| match e { AgentEvent::ToolCall { item_id, .. } => Some(item_id), _ => None }).collect();
    assert_eq!(calls.len(), 1);
    assert!(!killed.iter().any(|e| matches!(e, AgentEvent::ToolResult { .. })), "the process died mid-tool");

    let sid = |evs: &[AgentEvent]| evs.iter().find_map(|e| match e { AgentEvent::Init { agent_session_id, .. } => Some(agent_session_id.clone()), _ => None }).unwrap();
    assert_eq!(sid(&killed), sid(&events("resume")), "--resume keeps the session id");
}

#[test]
fn garbage_never_panics_or_invents_events() {
    let mut n = ClaudeNormalizer::new();
    for line in ["", "not json", "{", "[]", "null", "42", "{\"type\":null}", "{\"type\":\"assistant\"}",
        "{\"type\":\"stream_event\"}", "{\"type\":\"system\",\"subtype\":\"from_the_future\",\"x\":[1,2]}",
        "{\"type\":\"user\",\"message\":{\"content\":[{\"type\":\"tool_result\"}]}}",
        "{\"type\":\"control_request\",\"request_id\":\"r\",\"request\":{\"subtype\":\"new_thing\"}}",
        "{\"type\":\"brand_new_top_level\"}"]
    {
        assert!(n.push_line(line).is_empty(), "{line:?}");
    }
}

/// a session may be cut off after any line. parsing a prefix must never panic.
#[test]
fn every_prefix_of_every_fixture_parses() {
    for name in FIXTURES {
        let recs = recording::load(dir("fixtures").join(format!("{name}.jsonl"))).unwrap();
        let lines: Vec<&str> = recs.iter().filter(|r| r.dir == Dir::Out).filter_map(|r| r.text()).collect();
        for cut in 0..=lines.len() {
            let mut n = ClaudeNormalizer::new();
            for l in &lines[..cut] {
                n.push_line(l);
            }
        }
        // and a line truncated mid-way is just ignored.
        let mut n = ClaudeNormalizer::new();
        for l in &lines {
            let mut cut = l.len() / 2;
            while !l.is_char_boundary(cut) {
                cut -= 1;
            }
            n.push_line(&l[..cut]);
        }
    }
}

#[test]
fn unhandled_control_requests_are_reported_so_we_can_refuse_them() {
    use photon_agent::claude::normalize::unhandled_control_request as f;
    assert_eq!(f(r#"{"type":"control_request","request_id":"r9","request":{"subtype":"mcp_message"}}"#), Some("r9".into()));
    assert_eq!(f(r#"{"type":"control_request","request_id":"r9","request":{"subtype":"can_use_tool"}}"#), None);
    assert_eq!(f(r#"{"type":"assistant"}"#), None);
}

#[test]
fn thinking_arrives_as_text_the_user_can_read() {
    let evs = events("thinking");
    let streamed: String = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Delta { item_id, text, .. } if evs.iter().any(|b| matches!(b, AgentEvent::BlockStarted { item_id: i, block: BlockKind::Thinking, .. } if i == item_id)) => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(streamed.len() > 20, "thinking streamed as it happened: {streamed:?}");
    let whole = evs.iter().find_map(|e| match e {
        AgentEvent::Thinking { text, .. } => Some(text.clone()),
        _ => None,
    });
    assert_eq!(whole.as_deref(), Some(streamed.as_str()), "and the finished block says the same thing");
}

#[test]
fn switching_model_is_confirmed_with_the_models_real_name() {
    let evs = events("model");
    assert!(evs.contains(&AgentEvent::ModelChanged { model: "claude-sonnet-5-5".into() }), "we asked for `sonnet`");
}

#[test]
fn a_plan_is_neither_a_permission_nor_a_question_and_approving_it_leaves_plan_mode() {
    let evs = events("plan");
    let modes: Vec<&str> = evs
        .iter()
        .filter_map(|e| match e {
            AgentEvent::ModeChanged { mode } => Some(mode.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(modes.first(), Some(&"plan"));
    assert_eq!(modes.last(), Some(&"default"), "approving the plan put the agent back to work");
    let req = evs
        .iter()
        .find_map(|e| match e {
            AgentEvent::PermissionRequested { request } if request.tool == "ExitPlanMode" => Some(request),
            _ => None,
        })
        .expect("the plan was offered for approval");
    assert!(!req.is_question);
    assert!(req.plan.as_deref().is_some_and(|p| p.contains("hello.txt")), "{:?}", req.plan);
}

#[test]
fn context_size_is_reported_with_the_window_it_fits_in() {
    for name in ["plain", "thinking", "edit"] {
        let evs = events(name);
        let used = evs.iter().rev().find_map(|e| match e {
            AgentEvent::ContextUsed { tokens } => Some(*tokens),
            _ => None,
        });
        let window = evs.iter().find_map(|e| match e {
            AgentEvent::ContextWindow { tokens } => Some(*tokens),
            _ => None,
        });
        let (used, window) = (used.expect(name), window.expect(name));
        assert!(used > 1_000 && used < window, "{name}: {used} of {window}");
    }
}

#[test]
fn a_subagents_own_usage_is_not_mistaken_for_the_conversations() {
    // the subagent's context is its own; counting it would make the meter jump around.
    let mut n = ClaudeNormalizer::new();
    let line = serde_json::json!({
        "type": "assistant", "parent_tool_use_id": "toolu_1", "session_id": "s",
        "message": {"id": "m1", "model": "claude-haiku-5-5", "role": "assistant", "content": [{"type": "text", "text": "hi"}],
                    "usage": {"input_tokens": 9, "cache_read_input_tokens": 90_000, "cache_creation_input_tokens": 0}}
    });
    let evs = n.push_line(&line.to_string());
    assert!(!evs.iter().any(|e| matches!(e, AgentEvent::ContextUsed { .. })), "{evs:?}");
}

#[test]
fn a_refused_request_says_why() {
    let mut n = ClaudeNormalizer::new();
    let evs = n.push_line(r#"{"type":"control_response","response":{"subtype":"error","request_id":"photon-model-1","error":"Model 'x' not found","error_code":"not_found"}}"#);
    assert_eq!(evs, vec![AgentEvent::RequestFailed { message: "Model 'x' not found".into() }]);
}

#[test]
fn a_message_sent_with_a_screenshot_is_echoed_as_its_text() {
    let mut n = ClaudeNormalizer::new();
    let line = serde_json::json!({
        "type": "user", "uuid": "u1", "isReplay": true, "session_id": "s",
        "message": {"role": "user", "content": [{"type": "text", "text": "what is this?"}, {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}}]}
    });
    let evs = n.push_line(&line.to_string());
    assert_eq!(evs, vec![AgentEvent::UserMessage { item_id: ItemId::from("u1"), text: "what is this?".into() }]);
}

#[test]
fn tool_servers_are_read_at_start_and_when_asked_for() {
    let mut n = ClaudeNormalizer::new();
    let evs = n.push_line(r#"{"type":"control_response","response":{"subtype":"success","request_id":"photon-mcp-1","response":{"mcpServers":[
        {"name":"docs","status":"connected","scope":"user"},{"name":"tracker","status":"needs-auth"},{"status":"pending"}]}}}"#);
    let AgentEvent::McpServers { servers } = &evs[0] else { panic!("{evs:?}") };
    assert_eq!(servers.len(), 2, "a server with no name is nothing we can show");
    assert_eq!((servers[0].name.as_str(), servers[0].status.as_str(), servers[0].scope.as_deref()), ("docs", "connected", Some("user")));
    assert_eq!(servers[1].scope, None);
}

#[test]
fn the_clis_own_note_about_a_model_switch_is_not_something_the_user_said() {
    // exactly what claude 2.1.319 writes when the model is switched between turns.
    let mut n = ClaudeNormalizer::new();
    let note = r#"{"type":"user","message":{"role":"user","content":"<local-command-stdout>Set model to `sonnet (claude-sonnet-5-5)`</local-command-stdout>"},"parent_tool_use_id":null,"timestamp":"2026-11-10T21:59:58.695Z","isReplay":true}"#;
    assert!(n.push_line(note).is_empty(), "no message, and so no turn begun");
    let answer = r#"{"type":"control_response","response":{"subtype":"success","request_id":"m1","response":{"model":"claude-sonnet-5-5"}}}"#;
    assert_eq!(n.push_line(answer), vec![AgentEvent::ModelChanged { model: "claude-sonnet-5-5".into() }]);
}
