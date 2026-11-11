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
pub mod local;
pub mod rpc;
pub mod state;
pub mod surface;
pub mod workspace;

pub use agent::*;
pub use ids::*;
pub use item::*;
pub use rpc::*;
pub use state::*;
pub use workspace::*;

/// bump major on a breaking wire change, minor on an additive one.
pub const PROTO_MAJOR: u16 = 0;
pub const PROTO_MINOR: u16 = 6;

/// milliseconds since the unix epoch.
pub type Millis = u64;
