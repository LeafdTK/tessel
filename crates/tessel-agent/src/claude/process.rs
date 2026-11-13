//! running claude and talking to it.
//!
//! split in two so the protocol handling can be tested without a process:
//! a `Transport` is just lines in, lines out, an exit code and a way to kill
//! it. `drive` puts the claude protocol on top of any transport.

use super::normalize::{ClaudeNormalizer, unhandled_control_request};
use super::wire;
use crate::backend::{Agent, AgentOutput, Backend, Released, SpawnOpts, Spawned};
use std::os::fd::OwnedFd;
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
    /// resolves after stdout closes; dropped unanswered when the process was
    /// let go of rather than ended.
    pub exit: oneshot::Receiver<Option<i32>>,
    pub pid: Option<u32>,
    pub kill: Arc<dyn Fn() + Send + Sync>,
    /// how to let go of the process, when it can be.
    pub releaser: Option<Releaser>,
}

type Give<T> = oneshot::Sender<oneshot::Sender<Option<T>>>;

/// asks each of the tasks around a process to stop and give back its pipe.
pub struct Releaser {
    pid: u32,
    stdin: Give<OwnedFd>,
    stdout: Give<(OwnedFd, Vec<u8>)>,
    stderr: Give<OwnedFd>,
}

impl Releaser {
    async fn release(self) -> Option<Released> {
        async fn ask<T>(give: Give<T>) -> Option<T> {
            let (tx, rx) = oneshot::channel();
            give.send(tx).ok()?;
            rx.await.ok().flatten()
        }
        let stdin = ask(self.stdin).await?;
        let (stdout, unread) = ask(self.stdout).await?;
        let stderr = ask(self.stderr).await?;
        Some(Released { pid: self.pid, stdin, stdout, stderr, unread })
    }
}

struct ClaudeAgent {
    stdin: mpsc::UnboundedSender<String>,
    kill: Arc<dyn Fn() + Send + Sync>,
    pid: Option<u32>,
    next_request: AtomicU64,
    releaser: std::sync::Mutex<Option<Releaser>>,
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

    fn release(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Released>> + Send>> {
        let releaser = self.releaser.lock().unwrap().take();
        Box::pin(async move { releaser?.release().await })
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
        releaser: std::sync::Mutex::new(transport.releaser),
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
        // let go of rather than ended: the next daemon carries on, and nothing is said.
        if let Ok(code) = transport.exit.await {
            let _ = out_tx.send(AgentOutput::Exited { code });
        }
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
            // visual mode: the person watches, so the agent is told. nothing
            // else changes for it: its own Bash is what they see run.
            if opts.visual
                && let Some(i) = args.iter().position(|a| a.starts_with(wire::HOST_NOTE))
            {
                args[i].push_str(wire::VISUAL_NOTE);
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

    fn adopt(&self, r: Released, raw_log: Option<PathBuf>) -> anyhow::Result<Spawned> {
        use tokio::net::unix::pipe;
        let stdin = pipe::Sender::from_owned_fd(r.stdin).context("the agent's input")?;
        let stdout = pipe::Receiver::from_owned_fd(r.stdout).context("the agent's output")?;
        let stderr = pipe::Receiver::from_owned_fd(r.stderr).context("the agent's errors")?;
        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<String>();
        let stderr_log = raw_log.as_ref().map(|p| p.with_file_name("stderr.log"));
        // what was written before the handover is read first.
        let stdout = tokio::io::AsyncReadExt::chain(std::io::Cursor::new(r.unread), stdout);
        let (releaser, exit_rx, exited, stdout_rx) = pipes(r.pid, stdin, stdout, stderr, stdin_rx, stderr_log, None);
        let kill = killer(Some(r.pid), exited);
        Ok(drive(Transport { stdin: stdin_tx, stdout: stdout_rx, exit: exit_rx, pid: Some(r.pid), kill, releaser: Some(releaser) }, raw_log))
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
        let child_stdin = child.stdin.take().context("no stdin")?;
        let child_stdout = child.stdout.take().context("no stdout")?;
        let child_stderr = child.stderr.take().context("no stderr")?;

        let (stdin_tx, stdin_rx) = mpsc::unbounded_channel::<String>();
        for line in Self::first_lines(&opts) {
            let _ = stdin_tx.send(line);
        }
        let stderr_log = opts.raw_log.as_ref().map(|p| p.with_file_name("stderr.log"));
        let (releaser, exit_rx, exited, stdout_rx) = pipes(pid.unwrap_or(0), child_stdin, child_stdout, child_stderr, stdin_rx, stderr_log, Some(child));
        let kill = killer(pid, exited);
        Ok(drive(Transport { stdin: stdin_tx, stdout: stdout_rx, exit: exit_rx, pid, kill, releaser: pid.map(|_| releaser) }, opts.raw_log))
    }
}

/// a pipe that can be given back as a file descriptor.
trait IntoFd {
    fn into_fd(self) -> Option<OwnedFd>;
}
impl IntoFd for tokio::process::ChildStdin {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_owned_fd().ok()
    }
}
impl IntoFd for tokio::process::ChildStdout {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_owned_fd().ok()
    }
}
impl IntoFd for tokio::process::ChildStderr {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_owned_fd().ok()
    }
}
impl IntoFd for tokio::net::unix::pipe::Sender {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_blocking_fd().ok()
    }
}
impl IntoFd for tokio::net::unix::pipe::Receiver {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_blocking_fd().ok()
    }
}
impl<R: IntoFd + tokio::io::AsyncRead> IntoFd for tokio::io::Chain<std::io::Cursor<Vec<u8>>, R> {
    fn into_fd(self) -> Option<OwnedFd> {
        self.into_inner().1.into_fd()
    }
}

type Pipes = (Releaser, oneshot::Receiver<Option<i32>>, Arc<AtomicBool>, mpsc::UnboundedReceiver<String>);

/// the three tasks around an agent's pipes: lines in, lines out, errors to
/// the log. each stops and gives its pipe back when asked, for a handover.
/// `child`: when it's ours, waited on for its exit code; when it was taken
/// over, its end is seen as its output ending and the process going.
fn pipes<I, O, E>(
    pid: u32,
    mut stdin: I,
    stdout: O,
    stderr: E,
    mut lines_in: mpsc::UnboundedReceiver<String>,
    stderr_log: Option<PathBuf>,
    child: Option<tokio::process::Child>,
) -> Pipes
where
    I: tokio::io::AsyncWrite + IntoFd + Unpin + Send + 'static,
    O: tokio::io::AsyncRead + IntoFd + Unpin + Send + 'static,
    E: tokio::io::AsyncRead + IntoFd + Unpin + Send + 'static,
{
    let (give_in, mut want_in) = oneshot::channel::<oneshot::Sender<Option<OwnedFd>>>();
    let (give_out, mut want_out) = oneshot::channel::<oneshot::Sender<Option<(OwnedFd, Vec<u8>)>>>();
    let (give_err, mut want_err) = oneshot::channel::<oneshot::Sender<Option<OwnedFd>>>();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<String>();
    let (exit_tx, exit_rx) = oneshot::channel();
    let exited = Arc::new(AtomicBool::new(false));

    tokio::spawn(async move {
        loop {
            tokio::select! {
                line = lines_in.recv() => {
                    let Some(line) = line else { return };
                    let mut buf = line.into_bytes();
                    buf.push(b'\n');
                    if stdin.write_all(&buf).await.is_err() || stdin.flush().await.is_err() {
                        return;
                    }
                }
                Ok(back) = &mut want_in => {
                    let _ = back.send(stdin.into_fd());
                    return;
                }
            }
        }
    });

    // stderr must be drained or a chatty agent blocks. kept beside the raw log.
    tokio::spawn(async move {
        let mut file = stderr_log.and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok());
        let mut lines = BufReader::new(stderr).lines();
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if let Some(f) = file.as_mut() {
                            let _ = writeln!(f, "{line}");
                        }
                    }
                    _ => return,
                },
                Ok(back) = &mut want_err => {
                    let _ = back.send(lines.into_inner().into_inner().into_fd());
                    return;
                }
            }
        }
    });

    let exited_flag = exited.clone();
    tokio::spawn(async move {
        let mut lines = BufReader::new(stdout).lines();
        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if stdout_tx.send(line).is_err() {
                            break;
                        }
                    }
                    _ => break,
                },
                // `next_line` is cancel safe: a line half read stays in the buffer, and goes along.
                Ok(back) = &mut want_out => {
                    let buf = lines.into_inner();
                    let unread = buf.buffer().to_vec();
                    let _ = back.send(buf.into_inner().into_fd().map(|fd| (fd, unread)));
                    // exit_tx is dropped unanswered: let go of, not ended.
                    return;
                }
            }
        }
        drop(stdout_tx);
        let code = match child {
            Some(mut child) => child.wait().await.ok().and_then(|s| s.code()),
            None => {
                // not ours to wait on: its output ending is the sign; give it a moment to go.
                for _ in 0..50 {
                    if unsafe { libc::kill(pid as i32, 0) } != 0 {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                None
            }
        };
        exited_flag.store(true, Ordering::SeqCst);
        let _ = exit_tx.send(code);
    });

    (Releaser { pid, stdin: give_in, stdout: give_out, stderr: give_err }, exit_rx, exited, stdout_rx)
}

/// stop a process and everything it started: politely, then not.
fn killer(pid: Option<u32>, exited: Arc<AtomicBool>) -> Arc<dyn Fn() + Send + Sync> {
    Arc::new(move || {
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
    })
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

    /// a stand-in agent that says back every line it's given.
    fn echo_agent(dir: &std::path::Path) -> ClaudeBackend {
        let script = dir.join("echo-agent");
        std::fs::write(&script, "#!/bin/sh\nwhile IFS= read -r l; do printf '%s\\n' \"$l\"; done\n").unwrap();
        std::fs::set_permissions(&script, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        ClaudeBackend { program: script, env: None, extra_args: vec![] }
    }

    async fn next(out: &mut mpsc::UnboundedReceiver<AgentOutput>) -> Option<AgentOutput> {
        tokio::time::timeout(Duration::from_secs(5), out.recv()).await.ok().flatten()
    }

    #[tokio::test]
    async fn a_running_agent_is_let_go_of_and_taken_over_without_noticing() {
        let dir = std::env::temp_dir().join(format!("tessel-handoff-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let backend = echo_agent(&dir);
        let opts = SpawnOpts { cwd: dir.clone(), agent_session_id: "a".into(), resume: false, fork: false, model: None, permission_mode: None, effort: None, fast: false, visual: false, add_dirs: vec![], raw_log: None, tools: None, env: vec![] };
        let Spawned { agent, mut output } = backend.spawn(opts).unwrap();
        agent.send_user("before", &[]);
        assert!(matches!(next(&mut output).await, Some(AgentOutput::Events(_) | AgentOutput::Alive)), "it talks");
        let pid = agent.pid().unwrap();

        let released = agent.release().await.expect("let go of");
        assert_eq!(released.pid, pid);
        drop(agent);
        // the old side goes quiet without saying it ended, and the process lives on.
        assert!(next(&mut output).await.is_none(), "no exit is reported for a handover");
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, 0, "still running");

        let Spawned { agent, mut output } = backend.adopt(released, None).unwrap();
        assert_eq!(agent.pid(), Some(pid));
        agent.send_user("after", &[]);
        assert!(matches!(next(&mut output).await, Some(AgentOutput::Events(_) | AgentOutput::Alive)), "the same process answers the new side");

        agent.kill();
        loop {
            match next(&mut output).await {
                Some(AgentOutput::Exited { .. }) => break,
                Some(_) => continue,
                None => panic!("its end is seen even though it isn't ours to wait on"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

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
        let opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: Some("xhigh".into()), fast: true, visual: false, add_dirs: vec![], raw_log: None, tools: None, env: vec![] };
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
        let opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: None, fast: false, visual: false, add_dirs: vec![], raw_log: None, tools: Some(tools), env: vec![] };
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
        assert!(ClaudeBackend::first_lines(&SpawnOpts { tools: None, ..opts.clone() }).is_empty());

        // visual mode: told it's watched; its tools stay as they are.
        let watched = ClaudeBackend::args(&SpawnOpts { visual: true, ..opts.clone() });
        assert!(watched.windows(2).any(|w| w[0] == "--append-system-prompt" && w[1].contains("Visual mode is on") && w[1].contains("Use Bash as you always would")));
        assert!(!watched.iter().any(|a| a == "--disallowedTools"), "Bash stays: it's what they watch");
        assert!(!ClaudeBackend::args(&opts).iter().any(|a| a.contains("Visual mode")), "and nothing of it otherwise");
    }

    #[test]
    fn first_spawn_names_the_session_and_later_ones_resume_it() {
        let mut opts = SpawnOpts { cwd: "/tmp".into(), agent_session_id: "abc".into(), resume: false, fork: false, model: None, permission_mode: None, effort: None, fast: false, visual: false, add_dirs: vec![], raw_log: None, tools: None, env: vec![] };
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
