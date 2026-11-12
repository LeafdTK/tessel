//! checkpoints: the workspace's files as they were before each message, so a
//! turn's changes can be taken back.
//!
//! each is a git commit no branch points at, kept under
//! `refs/tessel/checkpoints/<session>/<ms>`. it's made through an index file
//! of its own (seeded from the real one, so only changed files are read), so
//! taking one never touches the branch, HEAD, the stash or what's staged; git
//! stores only what changed since the last, so each costs next to nothing.
//!
//! going back writes the files from a checkpoint and removes the ones it
//! didn't have, again without touching the index or history, after taking a
//! checkpoint of how things were, so going back can be undone.

use std::path::{Path, PathBuf};
use std::time::Duration;
use tessel_proto::{Millis, SessionId};

/// how many a session keeps, newest first.
const KEEP: usize = 50;

async fn git(dir: &Path, index: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).kill_on_drop(true);
    if let Some(i) = index {
        cmd.env("GIT_INDEX_FILE", i);
    }
    let out = tokio::time::timeout(Duration::from_secs(60), cmd.output()).await.map_err(|_| "git took too long".to_owned())?.map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

fn prefix(session: SessionId) -> String {
    format!("refs/tessel/checkpoints/{session}")
}

/// an index of our own, starting from the real one so unchanged files aren't read again.
async fn scratch_index(ws: &Path) -> Result<PathBuf, String> {
    let git_dir = PathBuf::from(git(ws, None, &["rev-parse", "--absolute-git-dir"]).await?);
    let real = git_dir.join("index");
    let ours = git_dir.join(format!("tessel-checkpoint-{}", uuid::Uuid::new_v4().simple()));
    if real.is_file() {
        std::fs::copy(&real, &ours).map_err(|e| e.to_string())?;
    }
    Ok(ours)
}

/// the workspace's files now, as a commit. none: not a git repository.
async fn capture(ws: &Path) -> Result<String, String> {
    let index = scratch_index(ws).await?;
    let result = async {
        git(ws, Some(&index), &["add", "-A"]).await?;
        let tree = git(ws, Some(&index), &["write-tree"]).await?;
        git(ws, None, &["commit-tree", &tree, "-m", "tessel checkpoint"]).await
    }
    .await;
    let _ = std::fs::remove_file(&index);
    result
}

/// take a checkpoint, and let go of ones past the newest `KEEP`.
pub async fn take(ws: &Path, session: SessionId, ms: Millis) -> Result<(), String> {
    if git(ws, None, &["rev-parse", "--is-inside-work-tree"]).await.is_err() {
        return Ok(());
    }
    let commit = capture(ws).await?;
    git(ws, None, &["update-ref", &format!("{}/{ms}", prefix(session)), &commit]).await?;
    let mut all = list(ws, session).await;
    if all.len() > KEEP {
        all.sort_unstable();
        for (ms, _) in &all[..all.len() - KEEP] {
            let _ = git(ws, None, &["update-ref", "-d", &format!("{}/{ms}", prefix(session))]).await;
        }
    }
    Ok(())
}

/// every checkpoint of a session: when, and its commit.
async fn list(ws: &Path, session: SessionId) -> Vec<(Millis, String)> {
    let out = git(ws, None, &["for-each-ref", "--format=%(refname) %(objectname)", &prefix(session)]).await.unwrap_or_default();
    out.lines()
        .filter_map(|l| {
            let (name, sha) = l.split_once(' ')?;
            Some((name.rsplit('/').next()?.parse().ok()?, sha.to_owned()))
        })
        .collect()
}

/// make the files those of `commit`: write its files, remove what it didn't have.
async fn restore(ws: &Path, commit: &str) -> Result<(), String> {
    let index = scratch_index(ws).await?;
    let result = async {
        // what's here now that the checkpoint didn't have (not counting what git ignores).
        git(ws, Some(&index), &["add", "-A"]).await?;
        let now: std::collections::HashSet<String> = git(ws, Some(&index), &["ls-files"]).await?.lines().map(str::to_owned).collect();
        git(ws, Some(&index), &["read-tree", commit]).await?;
        let then: std::collections::HashSet<String> = git(ws, Some(&index), &["ls-files"]).await?.lines().map(str::to_owned).collect();
        git(ws, Some(&index), &["checkout-index", "-a", "-f"]).await?;
        for gone in now.difference(&then) {
            let _ = std::fs::remove_file(ws.join(gone));
        }
        Ok(())
    }
    .await;
    let _ = std::fs::remove_file(&index);
    result
}

/// the files as they were before the turn that started at `started_ms`.
/// how things were just now is kept first, for `undo`.
pub async fn before(ws: &Path, session: SessionId, started_ms: Millis) -> Result<String, String> {
    let at = list(ws, session).await.into_iter().filter(|(ms, _)| *ms <= started_ms).max_by_key(|(ms, _)| *ms);
    let Some((_, commit)) = at else { return Err("there's no checkpoint from before that turn".into()) };
    let now = capture(ws).await?;
    git(ws, None, &["update-ref", &format!("refs/tessel/undo/{session}"), &now]).await?;
    restore(ws, &commit).await?;
    Ok("The files are back as they were before that turn.".into())
}

/// take back the last going-back.
pub async fn undo(ws: &Path, session: SessionId) -> Result<String, String> {
    let r = format!("refs/tessel/undo/{session}");
    let commit = git(ws, None, &["rev-parse", "--verify", "--quiet", &r]).await.map_err(|_| "there's nothing to undo".to_owned())?;
    restore(ws, &commit).await?;
    let _ = git(ws, None, &["update-ref", "-d", &r]).await;
    Ok("The files are back as they were.".into())
}

/// a session gone: its checkpoints with it.
pub async fn forget(ws: &Path, session: SessionId) {
    for (ms, _) in list(ws, session).await {
        let _ = git(ws, None, &["update-ref", "-d", &format!("{}/{ms}", prefix(session))]).await;
    }
    let _ = git(ws, None, &["update-ref", "-d", &format!("refs/tessel/undo/{session}")]).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    #[tokio::test]
    async fn a_turn_can_be_taken_back_without_touching_history_or_whats_staged() {
        let ws = std::env::temp_dir().join(format!("tessel-ckpt-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(&ws, args);
        }
        std::fs::write(ws.join(".gitignore"), "build/\n").unwrap();
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        std::fs::write(ws.join("gone.txt"), "keep me\n").unwrap();
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-q", "-m", "first"]);
        // something staged, and something not yet tracked, before the turn.
        std::fs::write(ws.join("a.txt"), "staged\n").unwrap();
        sh(&ws, &["add", "a.txt"]);
        std::fs::write(ws.join("draft.md"), "notes\n").unwrap();
        let (head, staged) = (sh(&ws, &["rev-parse", "HEAD"]), sh(&ws, &["diff", "--cached", "--name-only"]));
        let s = SessionId::new();
        take(&ws, s, 1000).await.unwrap();

        // the turn: edits, a new file, a deleted one, and a build folder git ignores.
        std::fs::write(ws.join("a.txt"), "the agent's\n").unwrap();
        std::fs::write(ws.join("new.rs"), "fn main() {}\n").unwrap();
        std::fs::remove_file(ws.join("gone.txt")).unwrap();
        std::fs::create_dir_all(ws.join("build")).unwrap();
        std::fs::write(ws.join("build/out"), "x").unwrap();
        take(&ws, s, 2000).await.unwrap();

        assert_eq!(before(&ws, s, 1500).await.unwrap(), "The files are back as they were before that turn.");
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "staged\n");
        assert_eq!(std::fs::read_to_string(ws.join("gone.txt")).unwrap(), "keep me\n");
        assert_eq!(std::fs::read_to_string(ws.join("draft.md")).unwrap(), "notes\n", "untracked work from before stays");
        assert!(!ws.join("new.rs").exists(), "what the turn added goes");
        assert!(ws.join("build/out").exists(), "what git ignores is left alone");
        assert_eq!((sh(&ws, &["rev-parse", "HEAD"]), sh(&ws, &["diff", "--cached", "--name-only"])), (head, staged), "history and staging untouched");
        assert!(sh(&ws, &["stash", "list"]).is_empty());

        // and going back can be undone.
        undo(&ws, s).await.unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "the agent's\n");
        assert!(ws.join("new.rs").exists() && !ws.join("gone.txt").exists());
        forget(&ws, s).await;
        assert!(sh(&ws, &["for-each-ref", "refs/tessel"]).is_empty());
        let _ = std::fs::remove_dir_all(&ws);
    }
}
