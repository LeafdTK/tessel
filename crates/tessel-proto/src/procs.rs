//! what the agents left running: every process started for a chat (by its
//! agent, its commands, or the tools it used), found by the environment tessel
//! gives them, so it's found even after the agent that started it is gone.

use crate::SessionId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentProcess {
    pub pid: i32,
    pub ppid: i32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// what it is, short: its program's name ("node", "Google Chrome").
    pub name: String,
    pub command: String,
    pub kind: ProcessKind,
    /// how long it's been running, in seconds.
    pub age_secs: u64,
    /// memory, its own and everything under it.
    pub rss_bytes: u64,
    /// cpu, percent of one core, it and everything under it.
    pub cpu: f32,
    /// ports it listens on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<u16>,
    /// how many processes are under it (a browser's helpers, a server's workers).
    #[serde(default)]
    pub children: u32,
    /// its agent is gone, or it was let go of: nobody will stop it but you.
    #[serde(default)]
    pub left_behind: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProcessKind {
    /// a browser an agent's tools opened.
    Browser,
    /// in one of tessel's terminals.
    Terminal,
    /// something an agent ran: a server, a build, a watcher. (and anything a newer daemon names.)
    #[default]
    #[serde(other)]
    Command,
}
