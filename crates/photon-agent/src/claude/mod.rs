//! claude code, driven as `claude -p` in bidirectional stream-json mode.
//!
//! everything in here was written against recorded output from claude code
//! 2.1.319 (see tests/fixtures), not from memory. when the cli changes shape,
//! re-record and let the golden tests say what moved.

pub mod normalize;
pub mod process;
pub mod wire;

pub use normalize::ClaudeNormalizer;
pub use process::{ClaudeBackend, Transport, drive};
