//! running claude and talking to it.
//!
//! split in two so the protocol handling can be tested without a process:
//! a `Transport` is just lines in, lines out, an exit code and a way to kill
//! it. `drive` puts the claude protocol on top of any transport.

use super::normalize::{ClaudeNormalizer, unhandled_control_request};
use super::wire;
use crate::backend::{Agent, AgentOutput, Backend, SpawnOpts, Spawned};
use anyhow::Context;
use tessel_proto::{Attachment, ModelInfo, PermissionDecision, PermissionRequest};
use std::io::Write;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::{mpsc, oneshot};

pub struct Transport {
    /// lines we write to the agent (no trailing newline).
    pub stdin: mpsc::UnboundedSender<String>,
    /// lines the agent writes. closes when the agent's output ends.
    pub stdout: mpsc::UnboundedReceiver<String>,
    /// resolves after stdout closes.
    pub exit: oneshot::Receiver<Option<i32>>,
    pub pid: Option<u32>,
    pub kill: Arc<dyn Fn() + Send + Sync>,
}

struct ClaudeAgent {
    stdin: mpsc::UnboundedSender<String>,
    kill: Arc<dyn Fn() + Send + Sync>,
    pid: Option<u32>,
    next_request: AtomicU64,
}

impl ClaudeAgent {
    fn write(&self, v: serde_json::Value) {
        // if the process is gone the send fails; the Exited event tells the story.
        let _ = self.stdin.send(v.to_string());
    }
}

impl Agent for ClaudeAgent {
    fn send_user(&self, text: &str, attachments: &[Attachment]) {
        self.write(wire::user_message(text, attachments));
    }

    fn set_model(&self, model: &str) {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        self.write(wire::set_model(&format!("tessel-model-{n}"), model));
    }

    fn set_mode(&self, mode: &str) {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        self.write(wire::set_permission_mode(&format!("tessel-mode-{n}"), mode));
    }

    fn apply_settings(&self, settings: &serde_json::Value) {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        self.write(wire::apply_flag_settings(&format!("tessel-flags-{n}"), settings));
    }

    fn mcp_status(&self) {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        self.write(wire::mcp_status(&format!("tessel-mcp-{n}")));
    }

    fn answer(&self, request: &PermissionRequest, decision: &PermissionDecision) {
        self.write(wire::permission_response(request, decision));
    }

    fn interrupt(&self) {
        let n = self.next_request.fetch_add(1, Ordering::Relaxed);
        self.write(wire::interrupt(&format!("tessel-interrupt-{n}")));
    }

    fn kill(&self) {
        (self.kill)();
    }

    fn pid(&self) -> Option<u32> {
        self.pid
    }
}

/// speak the claude protocol over a transport.
pub fn drive(mut transport: Transport, raw_log: Option<PathBuf>) -> Spawned {
    let (out_tx, out_rx) = mpsc::unbounded_channel();
    let stdin = transport.stdin.clone();
    let agent = ClaudeAgent {
        stdin: transport.stdin,
        kill: transport.kill,
        pid: transport.pid,
        next_request: AtomicU64::new(1),
    };
    tokio::spawn(async move {
        let mut normalizer = ClaudeNormalizer::new();
        let mut log = raw_log.and_then(|p| {
            if let Some(dir) = p.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::OpenOptions::new().create(true).append(true).open(p).ok()
        });
        while let Some(line) = transport.stdout.recv().await {
            if let Some(f) = log.as_mut() {
                // one write per line, so a crash can only ever lose a whole line.
                let mut buf = Vec::with_capacity(line.len() + 1);
                buf.extend_from_slice(line.as_bytes());
                buf.push(b'\n');
                let _ = f.write_all(&buf);
            }
            // never leave the cli waiting on a request we don't understand.
            if let Some(id) = unhandled_control_request(&line) {
                let _ = stdin.send(wire::control_error(&id, "tessel does not handle this request").to_string());
            }
            let events = normalizer.push_line(&line);
            let msg = if events.is_empty() { AgentOutput::Alive } else { AgentOutput::Events(events) };
            if out_tx.send(msg).is_err() {
                break;
            }
        }
        let code = transport.exit.await.ok().flatten();
        let _ = out_tx.send(AgentOutput::Exited { code });
    });
    Spawned { agent: Box::new(agent), output: out_rx }
}

/// the real thing.
pub struct ClaudeBackend {
    pub program: PathBuf,
    /// environment to run the agent in. `None` inherits ours.
    pub env: Option<Vec<(String, String)>>,
    /// appended to every invocation, e.g. `--permission-mode default`.
    pub extra_args: Vec<String>,
}

impl ClaudeBackend {
    pub fn args(opts: &SpawnOpts) -> Vec<String> {
        let mut args: Vec<String> = wire::base_args().into_iter().map(str::to_owned).collect();
        if opts.resume {
            args.extend(["--resume".to_owned(), opts.agent_session_id.clone()]);
            if opts.fork {
                args.push("--fork-session".to_owned());
            }
        } else {
            args.extend(["--session-id".to_owned(), opts.agent_session_id.clone()]);
        }
        if let Some(model) = &opts.model {
            args.extend(["--model".to_owned(), model.clone()]);
        }
        if let Some(effort) = &opts.effort {
            args.extend(["--effort".to_owned(), effort.clone()]);
        }
        if opts.fast {
            args.extend(["--settings".to_owned(), serde_json::json!({"fastMode": true}).to_string()]);
        }
        if let Some(mode) = &opts.permission_mode {
            args.extend(["--permission-mode".to_owned(), mode.clone()]);
        }
        for dir in &opts.add_dirs {
            args.extend(["--add-dir".to_owned(), dir.to_string_lossy().into_owned()]);
        }
        if opts.tools.is_some() {
            // the server itself is added once it's running (see `first_lines`).
            // tessel's own tools only touch tessel (its window, this session's
            // surfaces), so they don't stop to ask. the few that would need a
            // person are refused by the server itself.
            args.extend(["--allowedTools".to_owned(), "mcp__tessel".to_owned()]);
            // the note about how replies are shown grows a paragraph about the tools.
            if let Some(i) = args.iter().position(|a| a == wire::HOST_NOTE) {
                args[i] = format!("{}{}", wire::HOST_NOTE, wire::TOOLS_NOTE);
            }
        }
        args
    }

    /// what's written to the agent before anything else: tessel's tool server,
    /// added while the agent starts rather than on its command line.
    pub fn first_lines(opts: &SpawnOpts) -> Vec<String> {
        let Some(tools) = &opts.tools else { return Vec::new() };
        let env: serde_json::Map<String, serde_json::Value> = tools.env.iter().map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone()))).collect();
        // always loaded: not deferred behind the agent's tool search, where a
        // tool it isn't told about is a tool it never thinks to use.
        let servers = serde_json::json!({"tessel": {"type": "stdio", "command": tools.command, "args": ["mcp"], "env": env, "alwaysLoad": true}});
        vec![wire::mcp_set_servers("tessel-tools", &servers).to_string()]
    }
}

/// pick the model list out of the answer to `initialize`.
pub fn parse_models(line: &str, request_id: &str) -> Option<Vec<ModelInfo>> {
    parse_catalog(line, request_id).map(|c| c.models)
}

/// the models (with their effort levels and fast mode) and slash commands an
/// `initialize` answer lists.
pub fn parse_catalog(line: &str, request_id: &str) -> Option<tessel_proto::Catalog> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let r = v.get("response").filter(|_| v.get("type").and_then(|t| t.as_str()) == Some("control_response"))?;
    if r.get("request_id").and_then(|i| i.as_str()) != Some(request_id) {
        return None;
    }
    let text = |m: &serde_json::Value, k: &str| m.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
    let answer = r.get("response")?;
    let models = answer
        .get("models")?
        .as_array()?
        .iter()
        .filter(|m| m.get("value").and_then(|x| x.as_str()).is_some_and(|x| !x.is_empty()))
        .map(|m| ModelInfo {
            value: text(m, "value"),
            id: text(m, "resolvedModel"),
            name: text(m, "displayName"),
            description: text(m, "description"),
            efforts: if m.get("supportsEffort").and_then(|x| x.as_bool()) == Some(true) {
                m.get("supportedEffortLevels").and_then(|x| x.as_array()).into_iter().flatten().filter_map(|x| x.as_str().map(str::to_owned)).collect()
            } else {
                Vec::new()
            },
            fast: m.get("supportsFastMode").and_then(|x| x.as_bool()) == Some(true),
        })
        .collect();
    let commands = answer
        .get("commands")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let name = text(c, "name");
            (!name.is_empty()).then(|| tessel_proto::SlashCommand { name, description: text(c, "description"), hint: text(c, "argumentHint") })
        })
        .collect();
    Some(tessel_proto::Catalog { models, commands })
}

impl Backend for ClaudeBackend {
    /// start a throwaway agent, ask it what it can run as, and stop it. no
    /// message is sent, so nothing is spent.
    fn one_shot(&self, prompt: String, model: &str) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<String>> + Send>> {
        let mut cmd = tokio::process::Command::new(&self.program);
        // the question goes on stdin: a diff can be longer than an argument may be.
        cmd.args(["-p", "--model", model, "--output-format", "text", "--tools", ""])
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        Box::pin(async move {
            let mut child = cmd.spawn().context("could not start the agent")?;
            let mut stdin = child.stdin.take().context("no stdin")?;
            stdin.write_all(prompt.as_bytes()).await?;
            drop(stdin);
            let out = tokio::time::timeout(std::time::Duration::from_secs(90), child.wait_with_output()).await.context("the agent took too long")??;
            if !out.status.success() {
                anyhow::bail!("the agent failed: {}", String::from_utf8_lossy(&out.stderr).trim());
            }
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
        })
    }

    fn models(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<tessel_proto::Catalog>> + Send>> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose"])
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        Box::pin(async move {
            let mut child = cmd.spawn().context("could not start the agent to ask for its models")?;
            let mut stdin = child.stdin.take().context("no stdin")?;
            let stdout = child.stdout.take().context("no stdout")?;
            let ask = wire::initialize("tessel-models").to_string() + "\n";
            stdin.write_all(ask.as_bytes()).await?;
            stdin.flush().await?;
            let read = async {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    if let Some(models) = parse_catalog(&line, "tessel-models") {
                        return Some(models);
                    }
                }
                None
            };
            let models = tokio::time::timeout(Duration::from_secs(15), read).await;
            drop(stdin);
            let _ = child.kill().await;
            match models {
                Ok(Some(models)) => Ok(models),
                Ok(None) => anyhow::bail!("the agent exited without listing its models"),
                Err(_) => anyhow::bail!("the agent took too long to list its models"),
            }
        })
    }

    fn spawn(&self, opts: SpawnOpts) -> anyhow::Result<Spawned> {
        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(Self::args(&opts))
            .args(&self.extra_args)
            .current_dir(&opts.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // its own process group, so we can stop it *and* everything it started.
            .process_group(0);
        if let Some(env) = &self.env {
            cmd.env_clear().envs(env.iter().map(|(k, v)| (k, v)));
        }
        cmd.envs(opts.env.iter().map(|(k, v)| (k, v)));
        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not start {} in {}", self.program.display(), opts.cwd.display()))?;
        let pid = child.id();
        let mut child_stdin = child.stdin.take().context("no stdin")?;
        let child_stdout = child.stdout.take().context("no stdout")?;
        let child_stderr = child.stderr.take().context("no stderr")?;

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        for line in Self::first_lines(&opts) {
            let _ = stdin_tx.send(line);
        }
        let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exit_rx) = oneshot::channel();
        let exited = Arc::new(AtomicBool::new(false));

        tokio::spawn(async move {
            while let Some(line) = stdin_rx.recv().await {
                let mut buf = line.into_bytes();
                buf.push(b'\n');
                if child_stdin.write_all(&buf).await.is_err() || child_stdin.flush().await.is_err() {
                    break;
                }
            }
        });

        // stderr must be drained or a chatty agent blocks. keep it beside the raw log.
        let stderr_log = opts.raw_log.as_ref().map(|p| p.with_file_name("stderr.log"));
        tokio::spawn(async move {
            let mut file = stderr_log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
            let mut lines = BufReader::new(child_stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(f) = file.as_mut() {
                    let _ = writeln!(f, "{line}");
                }
            }
        });

        let exited_flag = exited.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(child_stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stdout_tx.send(line).is_err() {
                    break;
                }
            }
            drop(stdout_tx);
            let code = child.wait().await.ok().and_then(|s| s.code());
            exited_flag.store(true, Ordering::SeqCst);
            let _ = exit_tx.send(code);
        });

        let kill: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            let Some(pid) = pid else { return };
            if exited.load(Ordering::SeqCst) {
                return;
            }
            signal_group(pid, libc::SIGTERM);
            let exited = exited.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_secs(5));
                if !exited.load(Ordering::SeqCst) {
                    signal_group(pid, libc::SIGKILL);
                }
            });
        });

        Ok(drive(Transport { stdin: stdin_tx, stdout: stdout_rx, exit: exit_rx, pid, kill }, opts.raw_log))
    }
}

fn signal_group(pid: u32, signal: i32) {
    // negative pid = the whole process group. the agent was started as a group leader.
    unsafe {
        libc::kill(-(pid as i32), signal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_list_is_read_from_the_handshake_and_nothing_else() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"tessel-models","response":{"models":[
            {"value":"default","resolvedModel":"claude-opus-5-5","displayName":"Default (recommended)","description":"Opus 5.5"},
            {"value":"sonnet","displayName":"Sonnet 5.5"},{"value":""}]}}}"#;
        let models = parse_models(line, "tessel-models").unwrap();
        assert_eq!(models.len(), 2, "an entry with nothing to ask for is dropped");
        assert_eq!((models[0].value.as_str(), models[0].name.as_str(), models[0].description.as_str()), ("default", "Default (recommended)", "Opus 5.5"));
        assert_eq!((models[0].id.as_str(), models[1].id.as_str()), ("claude-opus-5-5", ""), "the id it resolves to, when the agent says");
        assert_eq!(parse_models(line, "someone-else"), None);
        assert_eq!(parse_models(r#"{"type":"system","subtype":"init"}"#, "tessel-models"), None);
    }

    #[test]
    fn effort_and_fast_mode_are_asked_for_at_start() {
        let opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: Some("xhigh".into()), fast: true, add_dirs: vec![], raw_log: None, tools: None, env: vec![] };
        let args = ClaudeBackend::args(&opts);
        assert!(args.windows(2).any(|w| w == ["--effort", "xhigh"]));
        let at = args.iter().position(|a| a == "--settings").expect("fast mode is a setting");
        assert_eq!(serde_json::from_str::<serde_json::Value>(&args[at + 1]).unwrap(), serde_json::json!({"fastMode": true}));
        let plain = ClaudeBackend::args(&SpawnOpts { effort: None, fast: false, ..opts });
        assert!(!plain.iter().any(|a| a == "--effort" || a == "--settings"));
    }

    #[test]
    fn the_catalog_keeps_each_model_s_effort_levels_fast_mode_and_the_slash_commands() {
        let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"q","response":{"models":[
            {"value":"opus","resolvedModel":"claude-opus-5-5","displayName":"Opus 5.5","description":"d","supportsEffort":true,"supportedEffortLevels":["low","high","max"],"supportsFastMode":true},
            {"value":"claude-haiku-4-5","resolvedModel":"claude-haiku-4-5","displayName":"Haiku 4.5","description":"d"}],
            "commands":[{"name":"review","description":"Review it","argumentHint":"<pr>"}]}}}"#;
        let c = parse_catalog(line, "q").unwrap();
        assert_eq!(c.models[0].efforts, ["low", "high", "max"]);
        assert!(c.models[0].fast && !c.models[1].fast);
        assert!(c.models[1].efforts.is_empty(), "no effort without supportsEffort");
        assert_eq!((c.commands[0].name.as_str(), c.commands[0].hint.as_str()), ("review", "<pr>"));
    }

    #[test]
    fn an_agent_given_tessels_tools_gets_them_scoped_to_its_session_and_is_told_why() {
        let tools = crate::backend::AgentTools { command: "/opt/tessel/tesselctl".into(), env: vec![("TESSEL_SESSION".into(), "s-1".into())] };
        let opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: None, fast: false, add_dirs: vec![], raw_log: None, tools: Some(tools), env: vec![] };
        let args = ClaudeBackend::args(&opts);
        // not on the command line: that makes the agent wait for every server before starting.
        assert!(!args.iter().any(|a| a == "--mcp-config"));
        let first = ClaudeBackend::first_lines(&opts);
        let req: serde_json::Value = serde_json::from_str(&first[0]).unwrap();
        assert_eq!(req["request"]["subtype"], "mcp_set_servers");
        let tessel = &req["request"]["servers"]["tessel"];
        assert_eq!(tessel["command"], "/opt/tessel/tesselctl");
        assert_eq!(tessel["args"][0], "mcp");
        assert_eq!(tessel["env"]["TESSEL_SESSION"], "s-1");
        assert_eq!(tessel["alwaysLoad"], true, "in front of the agent, not behind its tool search");
        assert!(args.windows(2).any(|w| w[0] == "--append-system-prompt" && w[1].contains("tessel_surface") && w[1].contains("```mermaid")), "one note, both parts");
        assert_eq!(args.iter().filter(|a| *a == "--append-system-prompt").count(), 1);
        assert!(args.windows(2).any(|w| w == ["--allowedTools", "mcp__tessel"]), "they run without a permission prompt");
        assert!(ClaudeBackend::first_lines(&SpawnOpts { tools: None, ..opts }).is_empty());
    }

    #[test]
    fn first_spawn_names_the_session_and_later_ones_resume_it() {
        let mut opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: None, fast: false, add_dirs: vec![], raw_log: None, tools: None, env: vec![] };
        let args = ClaudeBackend::args(&opts);
        assert!(args.windows(2).any(|w| w == ["--session-id", "abc"]));
        assert!(!args.iter().any(|a| a == "--resume"));
        assert!(args.windows(2).any(|w| w == ["--permission-prompt-tool", "stdio"]));

        opts.resume = true;
        opts.model = Some("haiku".into());
        opts.permission_mode = Some("plan".into());
        opts.add_dirs = vec!["/work/other".into(), "/work/third".into()];
        let args = ClaudeBackend::args(&opts);
        assert!(args.windows(2).any(|w| w == ["--resume", "abc"]));
        assert!(!args.iter().any(|a| a == "--session-id"));
        assert!(!args.iter().any(|a| a == "--fork-session"), "a resume carries on the same conversation");
        let forked = ClaudeBackend::args(&SpawnOpts { fork: true, ..opts.clone() });
        assert!(forked.windows(3).any(|w| w == ["--resume", "abc", "--fork-session"]), "a fork starts from it under a new id");
        assert!(args.windows(2).any(|w| w == ["--model", "haiku"]));
        assert!(args.windows(2).any(|w| w == ["--permission-mode", "plan"]));
        assert!(args.windows(2).any(|w| w == ["--add-dir", "/work/other"]) && args.windows(2).any(|w| w == ["--add-dir", "/work/third"]));
        assert!(args.windows(2).any(|w| w == ["--thinking-display", "summarized"]), "or thinking arrives empty");
        assert!(args.windows(2).any(|w| w[0] == "--append-system-prompt" && w[1].contains("```mermaid") && w[1].contains("$$")), "or the agent never learns it may draw");
    }
}
