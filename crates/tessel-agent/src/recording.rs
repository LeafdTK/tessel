//! recorded agent sessions: what the agent wrote, what we wrote, and when.
//! used by golden tests and by the fake backend that replays them.

use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dir {
    /// agent -> us (its stdout)
    Out,
    /// us -> agent (its stdin)
    In,
    /// notes from the recorder (scenario name, kill -9, exit code)
    Meta,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecLine {
    pub dir: Dir,
    /// ms since the recording began
    pub t: u64,
    pub line: serde_json::Value,
}

impl RecLine {
    /// the raw text line, for `Out`/`In`.
    pub fn text(&self) -> Option<&str> {
        self.line.as_str()
    }
}

pub fn load(path: impl AsRef<Path>) -> anyhow::Result<Vec<RecLine>> {
    let text = std::fs::read_to_string(path.as_ref())?;
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let rec: RecLine = serde_json::from_str(line)
            .map_err(|e| anyhow::anyhow!("{}:{}: {e}", path.as_ref().display(), n + 1))?;
        out.push(rec);
    }
    Ok(out)
}

/// just the agent's stdout lines, in order.
pub fn out_lines(recs: &[RecLine]) -> impl Iterator<Item = &str> {
    recs.iter()
        .filter(|r| r.dir == Dir::Out)
        .filter_map(|r| r.text())
}
