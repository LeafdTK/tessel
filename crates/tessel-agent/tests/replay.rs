//! the claude protocol driver, end to end, over a replayed recording.

use tessel_agent::replay::ReplayBackend;
use tessel_agent::{AgentOutput, Backend, SpawnOpts, recording};
use tessel_proto::{AgentEvent, PermissionDecision, TurnOutcome};
use std::path::PathBuf;
use std::time::Duration;

fn fixture(name: &str) -> Vec<recording::RecLine> {
    recording::load(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(format!("{name}.jsonl"))).unwrap()
}

fn opts(raw_log: Option<PathBuf>) -> SpawnOpts {
    SpawnOpts { cwd: "/tmp".into(), agent_session_id: "s".into(), resume: false, model: None, permission_mode: None, add_dirs: vec![], raw_log, tools: None }
}

async fn next(spawned: &mut tessel_agent::Spawned) -> AgentOutput {
    tokio::time::timeout(Duration::from_secs(5), spawned.output.recv()).await.expect("agent went quiet").expect("output closed")
}

#[tokio::test]
async fn a_permission_prompt_blocks_until_answered_then_the_turn_finishes() {
    let backend = ReplayBackend::new(vec![fixture("perm_allow")]);
    let mut s = backend.spawn(opts(None)).unwrap();

    // nothing happens until we say something.
    assert!(tokio::time::timeout(Duration::from_millis(50), s.output.recv()).await.is_err());
    s.agent.send_user("go", &[]);

    let request = loop {
        if let AgentOutput::Events(evs) = next(&mut s).await
            && let Some(AgentEvent::PermissionRequested { request }) = evs.into_iter().find(|e| matches!(e, AgentEvent::PermissionRequested { .. }))
        {
            break request;
        }
    };
    // and now it waits on us.
    assert!(tokio::time::timeout(Duration::from_millis(50), s.output.recv()).await.is_err());
    s.agent.answer(&request, &PermissionDecision::Allow { updated_input: None });

    let mut finished = false;
    loop {
        match next(&mut s).await {
            AgentOutput::Events(evs) => {
                finished |= evs.iter().any(|e| matches!(e, AgentEvent::TurnFinished { outcome: TurnOutcome::Completed, .. }));
                if finished && evs.iter().any(|e| matches!(e, AgentEvent::Phase { .. })) {
                    break;
                }
            }
            AgentOutput::Alive => {}
            AgentOutput::Exited { .. } => panic!("exited before the turn finished"),
        }
    }

    let written = backend.written.lock().unwrap().clone();
    assert_eq!(written.len(), 2);
    let answer: serde_json::Value = serde_json::from_str(&written[1]).unwrap();
    assert_eq!(answer["type"], "control_response");
    assert_eq!(answer["response"]["request_id"], request.request_id.0);
    assert_eq!(answer["response"]["response"]["behavior"], "allow");
    assert_eq!(answer["response"]["response"]["updatedInput"]["command"], "echo tessel-fixture > out.txt");
}

#[tokio::test]
async fn kill_ends_the_stream_with_exited_and_nothing_after() {
    let backend = ReplayBackend::new(vec![fixture("perm_allow")]);
    let mut s = backend.spawn(opts(None)).unwrap();
    s.agent.send_user("go", &[]);
    loop {
        if let AgentOutput::Events(evs) = next(&mut s).await
            && evs.iter().any(|e| matches!(e, AgentEvent::PermissionRequested { .. }))
        {
            break;
        }
    }
    s.agent.kill();
    loop {
        match next(&mut s).await {
            AgentOutput::Exited { code } => {
                assert_eq!(code, None);
                break;
            }
            _ => {}
        }
    }
    assert!(s.output.recv().await.is_none(), "Exited must be the last thing sent");
}

#[tokio::test]
async fn every_line_lands_in_the_raw_log_verbatim() {
    let dir = std::env::temp_dir().join(format!("tessel-raw-{}", std::process::id()));
    let log = dir.join("nested").join("raw.jsonl");
    let recs = fixture("plain");
    let backend = ReplayBackend::new(vec![recs.clone()]);
    let mut s = backend.spawn(opts(Some(log.clone()))).unwrap();
    s.agent.send_user("hi", &[]);
    loop {
        if let AgentOutput::Events(evs) = next(&mut s).await
            && evs.iter().any(|e| matches!(e, AgentEvent::Phase { phase: tessel_proto::AgentPhase::Idle }))
        {
            break;
        }
    }
    let want: Vec<&str> = recording::out_lines(&recs).collect();
    let got = std::fs::read_to_string(&log).unwrap();
    assert_eq!(got.lines().collect::<Vec<_>>(), want);
    let _ = std::fs::remove_dir_all(dir);
}
