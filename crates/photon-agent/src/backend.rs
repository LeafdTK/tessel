//! the seam between the daemon and any agent.

use photon_proto::{AgentEvent, Attachment, ModelInfo, PermissionDecision, PermissionRequest};
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
    pub model: Option<String>,
    /// start in this permission mode instead of the user's default.
    pub permission_mode: Option<String>,
    /// other folders the agent may work in besides `cwd`.
    pub add_dirs: Vec<PathBuf>,
    /// append every line the agent writes here, verbatim. this is the record
    /// of truth: everything else can be rebuilt from it.
    pub raw_log: Option<PathBuf>,
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
    /// ask for the state of the agent's tool servers. answered with `AgentEvent::McpServers`.
    fn mcp_status(&self);
    fn answer(&self, request: &PermissionRequest, decision: &PermissionDecision);
    fn interrupt(&self);
    /// stop the process and everything it started. polite first, then not.
    fn kill(&self);
    fn pid(&self) -> Option<u32>;
}

pub struct Spawned {
    pub agent: Box<dyn Agent>,
    pub output: mpsc::UnboundedReceiver<AgentOutput>,
}

/// must be called from inside a tokio runtime.
pub trait Backend: Send + Sync + 'static {
    fn spawn(&self, opts: SpawnOpts) -> anyhow::Result<Spawned>;
    /// the models this agent can run as. asked rarely; may take a second.
    fn models(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<Vec<ModelInfo>>> + Send>>;
}
