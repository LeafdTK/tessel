//! wire types shared by the daemon and every client.
//!
//! rules for this crate:
//! - no tokio, no gpui. it has to compile for a future web/ios client.
//! - every enum that crosses the wire tolerates variants it doesn't know.
//!   never `deny_unknown_fields`.
//! - a session has independent attributes, never a "kind". nothing here may
//!   grow an enum of session types.

pub mod activity;
pub mod agent;
pub mod frame;
pub mod ids;
pub mod item;
pub mod keys;
pub mod kube;
pub mod local;
pub mod procs;
pub mod proof;
pub mod browser;
pub mod rpc;
pub mod state;
pub mod surface;
pub mod task;
pub mod term;
pub mod voice;
pub mod workspace;
pub mod workflow;

pub use agent::*;
pub use browser::*;
pub use ids::*;
pub use item::*;
pub use rpc::*;
pub use state::*;
pub use workspace::*;
pub use workflow::{Choice, Input, InputKind, Options, Pick, Run, RunStep, Step, StepDoes, StepState, Workflow};

/// bump major on a breaking wire change, minor on an additive one.
pub const PROTO_MAJOR: u16 = 0;
pub const PROTO_MINOR: u16 = 35;

/// milliseconds since the unix epoch.
pub type Millis = u64;
