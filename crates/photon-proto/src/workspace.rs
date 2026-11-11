//! what a client can ask about a session's working directory: what changed,
//! what is in it, and where its pull request stands.
//!
//! the client never touches the filesystem or runs git itself. every path
//! here is relative to the session's working directory, and the daemon
//! refuses anything that would leave it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeStatus {
    Added,
    Modified,
    Deleted,
    /// new and not yet known to git.
    Untracked,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: String,
    pub status: ChangeStatus,
    pub added: u32,
    pub removed: u32,
    #[serde(default)]
    pub binary: bool,
}

/// everything that differs from where this work started.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSummary {
    /// false when the working directory is not in a git repository.
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// what the changes are measured against (for example `origin/main`).
    /// `None` means the last commit: only uncommitted work is listed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    /// commits on this branch that the base does not have.
    #[serde(default)]
    pub ahead: u32,
    #[serde(default)]
    pub files: Vec<FileChange>,
    #[serde(default)]
    pub added: u32,
    #[serde(default)]
    pub removed: u32,
}

/// one file, before and after.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileDiff {
    pub path: String,
    /// the file as it was at the base. `None` if it did not exist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old: Option<String>,
    /// the file as it is now. `None` if it has been deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new: Option<String>,
    /// the same change as a unified diff.
    #[serde(default)]
    pub unified: String,
    #[serde(default)]
    pub binary: bool,
    /// the file was too large to send whole.
    #[serde(default)]
    pub truncated: bool,
}

/// one commit in the history.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    pub sha: String,
    /// its parents' shas: none for the first commit, two or more for a merge.
    #[serde(default)]
    pub parents: Vec<String>,
    /// branches and tags pointing at it, as git decorates them (`HEAD -> main`, `origin/main`, `tag: v1`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refs: Vec<String>,
    pub author: String,
    /// when it was made, in ms since the epoch.
    pub time_ms: u64,
    pub subject: String,
}

/// a page of history, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitLog {
    pub commits: Vec<Commit>,
    /// there are older commits past this page.
    #[serde(default)]
    pub more: bool,
}

/// what is staged for the next commit, and what isn't yet.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkTree {
    pub is_repo: bool,
    #[serde(default)]
    pub staged: Vec<FileChange>,
    /// changed since staged, and files git doesn't know yet.
    #[serde(default)]
    pub unstaged: Vec<FileChange>,
    /// something photon did here that `GitUndo` would take back, said plainly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    #[serde(default)]
    pub size: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    pub path: String,
    /// `None` for a binary file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub binary: bool,
    #[serde(default)]
    pub truncated: bool,
    pub size: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckState {
    Pending,
    Passing,
    Failing,
    Skipped,
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub state: CheckState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub url: String,
    /// open, closed or merged, as the host reports it.
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub base: String,
    pub head: String,
    #[serde(default)]
    pub checks: Vec<Check>,
}

/// where the pull request for this branch stands, or why there is nothing to show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrStatus {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr: Option<PrSummary>,
    /// why `pr` is empty, in words for a person: no pull request yet, or the
    /// github cli is not installed, or it is not signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}
