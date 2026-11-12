//! the seam between the daemon and any agent.

use tessel_proto::{AgentEvent, Attachment, PermissionDecision, PermissionRequest};
use std::future::Future;
use std::pin::Pin;
use std::path::PathBuf;
use tokio::sync::mpsc;

#[derive(Clone, Debug)]
pub struct SpawnOpts {
    pub cwd: PathBuf,
    /// the agent's own conversation id. we choose it on first spawn.
    pub agent_session_id: String,
    /// continue an existing conversation instead of starting one.
    pub resume: bool,
    /// with `resume`: carry on from that conversation under a new id, leaving it as it was.
    pub fork: bool,
    pub model: Option<String>,
    /// start in this permission mode instead of the user's default.
    pub permission_mode: Option<String>,
    /// how hard it thinks; none: the model's default.
    pub effort: Option<String>,
    /// fast mode.
    pub fast: bool,
    /// visual mode: the person watches. told so, and without Bash unless `visual_bash`.
    pub visual: bool,
    pub visual_bash: bool,
    /// other folders the agent may work in besides `cwd`.
    pub add_dirs: Vec<PathBuf>,
    /// append every line the agent writes here, verbatim. this is the record
    /// of truth: everything else can be rebuilt from it.
    pub raw_log: Option<PathBuf>,
    /// tessel's own tools for this agent (the window, surfaces), if it gets them.
    pub tools: Option<AgentTools>,
    /// what it's told about its workspace: `TESSEL_PORT` and the like.
    pub env: Vec<(String, String)>,
}

/// how to start tessel's tool server for one agent.
#[derive(Clone, Debug)]
pub struct AgentTools {
    /// the `tesselctl` binary; it is run as `tesselctl mcp`.
    pub command: PathBuf,
    /// what it needs to find the daemon and its session.
    pub env: Vec<(String, String)>,
}

#[derive(Debug)]
pub enum AgentOutput {
    Events(Vec<AgentEvent>),
    /// the agent wrote something that isn't an event. still proof of life.
    Alive,
    /// always the last thing sent.
    Exited { code: Option<i32> },
}

/// a running agent. every method is fire-and-forget: results come back as events.
pub trait Agent: Send {
    fn send_user(&self, text: &str, attachments: &[Attachment]);
    fn set_model(&self, model: &str);
    fn set_mode(&self, mode: &str);
    /// change settings of the running agent (effort, fast mode), as claude's
    /// `apply_flag_settings` takes them: `{"effortLevel": "high", "fastMode": true}`.
    fn apply_settings(&self, settings: &serde_json::Value);
    /// ask for the state of the agent's tool servers. answered with `AgentEvent::McpServers`.
    fn mcp_status(&self);
    fn answer(&self, request: &PermissionRequest, decision: &PermissionDecision);
    fn interrupt(&self);
    /// stop the process and everything it started. polite first, then not.
    fn kill(&self);
    fn pid(&self) -> Option<u32>;
    /// stop talking to the process and hand over its pipes, leaving it
    /// running. none: this agent can't be handed over (it is then stopped).
    fn release(&self) -> Pin<Box<dyn Future<Output = Option<Released>> + Send>> {
        Box::pin(async { None })
    }
}

/// an agent process one daemon lets go of for the next to take over: its
/// pid, the pipes to it, and what it had written that hadn't been read yet.
/// the process never notices.
pub struct Released {
    pub pid: u32,
    pub stdin: std::os::fd::OwnedFd,
    pub stdout: std::os::fd::OwnedFd,
    pub stderr: std::os::fd::OwnedFd,
    pub unread: Vec<u8>,
}

pub struct Spawned {
    pub agent: Box<dyn Agent>,
    pub output: mpsc::UnboundedReceiver<AgentOutput>,
}

/// must be called from inside a tokio runtime.
pub trait Backend: Send + Sync + 'static {
    fn spawn(&self, opts: SpawnOpts) -> anyhow::Result<Spawned>;
    /// the models this agent can run as. asked rarely; may take a second.
    fn models(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<tessel_proto::Catalog>> + Send>>;
    /// one question, one answer, no session: for small jobs like a commit
    /// message. `model` is an alias (`haiku`) or an id.
    fn one_shot(&self, prompt: String, model: &str) -> Pin<Box<dyn Future<Output = anyhow::Result<String>> + Send>> {
        let _ = (prompt, model);
        Box::pin(async { anyhow::bail!("this agent can't answer one-off questions") })
    }
    /// take over an agent another daemon let go of, carrying on where it was.
    fn adopt(&self, released: Released, raw_log: Option<PathBuf>) -> anyhow::Result<Spawned> {
        let _ = (released, raw_log);
        anyhow::bail!("this agent can't be taken over")
    }
}
