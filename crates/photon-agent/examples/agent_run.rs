//! run one real claude turn and print the normalized events.
//!
//!   cargo run -p photon-agent --example agent_run -- [--allow] [--model haiku] "your prompt"
//!
//! runs in the current directory. permission prompts are denied unless --allow.

use photon_agent::claude::ClaudeBackend;
use photon_agent::{AgentOutput, Backend, SpawnOpts};
use photon_proto::{AgentEvent, AgentPhase, PermissionDecision};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut allow = false;
    let mut model = None;
    let mut prompt = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--allow" => allow = true,
            "--model" => model = args.next(),
            _ => prompt.push(a),
        }
    }
    anyhow::ensure!(!prompt.is_empty(), "usage: agent_run [--allow] [--model m] <prompt>");

    let backend = ClaudeBackend { program: "claude".into(), env: None, extra_args: Vec::new() };
    let mut spawned = backend.spawn(SpawnOpts {
        cwd: std::env::current_dir()?,
        agent_session_id: uuid::Uuid::new_v4().to_string(),
        resume: false,
        model,
        permission_mode: None, add_dirs: vec![],
        raw_log: None,
    })?;
    let started = std::time::Instant::now();
    spawned.agent.send_user(&prompt.join(" "), &[]);

    let mut finished = false;
    while let Some(out) = spawned.output.recv().await {
        let ms = started.elapsed().as_millis();
        match out {
            AgentOutput::Alive => {}
            AgentOutput::Exited { code } => {
                println!("{ms:>6}ms exited code={code:?}");
                break;
            }
            AgentOutput::Events(events) => {
                for ev in events {
                    println!("{ms:>6}ms {}", serde_json::to_string(&ev)?);
                    match ev {
                        AgentEvent::PermissionRequested { request } => {
                            let decision = if allow {
                                PermissionDecision::Allow { updated_input: None }
                            } else {
                                PermissionDecision::Deny { message: "agent_run: denied (pass --allow)".into() }
                            };
                            spawned.agent.answer(&request, &decision);
                        }
                        AgentEvent::TurnFinished { .. } => finished = true,
                        AgentEvent::Phase { phase: AgentPhase::Idle } if finished => spawned.agent.kill(),
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}
