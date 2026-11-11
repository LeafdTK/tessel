//! agent backends. each one turns a specific agent cli into `AgentEvent`s and
//! accepts a small set of commands. nothing outside this crate knows what
//! claude's json looks like.

pub mod backend;
pub mod claude;
pub mod recording;
pub mod replay;

pub use backend::{Agent, AgentOutput, Backend, SpawnOpts, Spawned};
