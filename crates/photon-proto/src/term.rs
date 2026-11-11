//! terminals the daemon runs: their description, and the binary frames that
//! carry their bytes (never json, never base64).
//!
//! a binary frame's payload is `u64 big-endian terminal id | bytes`. from the
//! daemon it is what the program wrote; to the daemon, what was typed.

use crate::SessionId;
use serde::{Deserialize, Serialize};

pub type TermId = u64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TermInfo {
    pub id: TermId,
    /// what the program calls itself (OSC 0/2); empty until it says.
    #[serde(default)]
    pub title: String,
    /// where the shell is (OSC 7), else where it started.
    pub cwd: String,
    pub cols: u16,
    pub rows: u16,
    /// the session it was opened for, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionId>,
    /// what it runs; empty for a login shell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// set once the program has exited: its status (signals as 128+n).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exited: Option<i32>,
}

/// a binary frame's payload for a terminal.
pub fn encode(term: TermId, bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(8 + bytes.len());
    out.extend_from_slice(&term.to_be_bytes());
    out.extend_from_slice(bytes);
    out
}

/// which terminal a binary payload is for, and its bytes.
pub fn decode(payload: &[u8]) -> Option<(TermId, &[u8])> {
    let (id, bytes) = payload.split_at_checked(8)?;
    Some((u64::from_be_bytes(id.try_into().ok()?), bytes))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_payload_names_its_terminal() {
        let p = super::encode(7, b"hi\x1b[0m");
        assert_eq!(super::decode(&p), Some((7, &b"hi\x1b[0m"[..])));
        assert_eq!(super::decode(b"short"), None);
    }
}
