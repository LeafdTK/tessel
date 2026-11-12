//! git worktrees: one way to give a session its own place to work.
//!
//! a worktree is just how a session's `cwd` got filled in. it does not make
//! the session a different kind of thing.

use tessel_proto::{ErrorCode, RpcError, WorktreeSpec};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created {
    /// the new working directory.
    pub path: String,
    /// the repository it belongs to.
    pub repo: String,
    pub branch: String,
}

async fn git(dir: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(timeout, cmd.output()).await {
        Err(_) => Err(format!("git {} timed out", args.join(" "))),
        Ok(Err(e)) => Err(format!("could not run git: {e}")),
        Ok(Ok(out)) if out.status.success() => Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned()),
        Ok(Ok(out)) => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
    }
}

const QUICK: Duration = Duration::from_secs(10);

/// the top of the repository holding `dir`, if it's in one.
pub async fn repo_root(dir: &str) -> Option<String> {
    git(Path::new(dir), &["rev-parse", "--show-toplevel"], QUICK).await.ok()
}

/// make a fresh worktree of the repo containing `cwd`, on a new branch.
pub async fn create(cwd: &str, spec: &WorktreeSpec, root: &Path, hint: &str) -> Result<Created, RpcError> {
    let cwd = Path::new(cwd);
    let repo = git(cwd, &["rev-parse", "--show-toplevel"], QUICK)
        .await
        .map_err(|_| RpcError::new(ErrorCode::Invalid, format!("{} is not inside a git repository", cwd.display())))?;
    let repo_path = PathBuf::from(&repo);

    let base = match &spec.base {
        Some(b) => b.clone(),
        None => default_base(&repo_path).await,
    };

    let name = match &spec.name {
        Some(n) => {
            let clean = slug(n);
            if clean.is_empty() {
                return Err(RpcError::new(ErrorCode::Invalid, format!("{n:?} is not a usable worktree name")));
            }
            clean
        }
        None => {
            let stem = slug(hint);
            let tag = &uuid::Uuid::new_v4().simple().to_string()[..5];
            if stem.is_empty() { format!("session-{tag}") } else { format!("{stem}-{tag}") }
        }
    };
    let branch = format!("tessel/{name}");
    let repo_name = repo_path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into());
    let path = root.join(repo_name).join(&name);

    if path.exists() {
        return Err(RpcError::new(ErrorCode::Conflict, format!("{} already exists", path.display())));
    }
    // a branch can only be checked out in one worktree, so never reuse one.
    if git(&repo_path, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{branch}")], QUICK).await.is_ok() {
        return Err(RpcError::new(ErrorCode::Conflict, format!("branch {branch} already exists")));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| RpcError::new(ErrorCode::Internal, format!("creating {}: {e}", parent.display())))?;
    }
    let path_str = path.to_string_lossy().into_owned();
    git(&repo_path, &["worktree", "add", "-b", &branch, &path_str, &base], Duration::from_secs(120))
        .await
        .map_err(|e| RpcError::new(ErrorCode::Invalid, format!("git worktree add failed: {e}")))?;
    Ok(Created { path: path_str, repo, branch })
}

/// the remote's default branch, freshly fetched when there is a remote;
/// otherwise wherever the repo is now.
async fn default_base(repo: &Path) -> String {
    if git(repo, &["remote", "get-url", "origin"], QUICK).await.is_ok() {
        // best effort: offline or slow must not stop a session from starting.
        let _ = git(repo, &["fetch", "--quiet", "origin"], Duration::from_secs(20)).await;
        if let Ok(head) = git(repo, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"], QUICK).await
            && !head.is_empty()
        {
            return head;
        }
    }
    "HEAD".to_owned()
}

/// remove a worktree the daemon made. the branch is left alone: it may hold
/// work nobody has merged.
pub async fn remove(repo: &str, path: &str) -> Result<(), String> {
    git(Path::new(repo), &["worktree", "remove", "--force", path], Duration::from_secs(60)).await.map(|_| ())
}

/// lowercase words joined by dashes, at most 32 chars. empty if nothing usable.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    out.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn repo(dir: &Path) {
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "t"],
            &["config", "user.email", "t@example.invalid"],
            &["commit", "-q", "--allow-empty", "-m", "init"],
        ] {
            git(dir, args, QUICK).await.unwrap();
        }
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix the flaky test!"), "fix-the-flaky-test");
        assert_eq!(slug("  --weird__name--  "), "weird-name");
        assert_eq!(slug("日本語"), "");
        assert!(slug(&"word ".repeat(40)).len() <= 32);
        assert!(!slug(&"word ".repeat(40)).ends_with('-'));
    }

    #[tokio::test]
    async fn creates_an_isolated_worktree_on_a_new_branch() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("myrepo");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        repo(&src).await;
        let root = tmp.path().join("ws");

        // from a subdirectory, with a generated name.
        let made = create(src.join("sub").to_str().unwrap(), &WorktreeSpec::default(), &root, "Fix login").await.unwrap();
        assert!(made.branch.starts_with("tessel/fix-login-"), "{}", made.branch);
        assert!(Path::new(&made.path).join(".git").exists());
        assert!(made.path.starts_with(root.join("myrepo").to_str().unwrap()));
        assert_eq!(git(Path::new(&made.path), &["rev-parse", "--abbrev-ref", "HEAD"], QUICK).await.unwrap(), made.branch);
        // the original checkout is untouched.
        assert_eq!(git(&src, &["rev-parse", "--abbrev-ref", "HEAD"], QUICK).await.unwrap(), "main");

        // a file written in the worktree does not appear in the original.
        std::fs::write(Path::new(&made.path).join("new.txt"), "x").unwrap();
        assert!(!src.join("new.txt").exists());

        remove(&made.repo, &made.path).await.unwrap();
        assert!(!Path::new(&made.path).exists());
        // the branch survives removal.
        assert!(git(&src, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{}", made.branch)], QUICK).await.is_ok());
    }

    #[tokio::test]
    async fn refuses_to_reuse_a_branch_or_a_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("r");
        std::fs::create_dir_all(&src).unwrap();
        repo(&src).await;
        let root = tmp.path().join("ws");
        let spec = WorktreeSpec { base: Some("main".into()), name: Some("Same Name".into()) };

        let first = create(src.to_str().unwrap(), &spec, &root, "").await.unwrap();
        assert_eq!(first.branch, "tessel/same-name");
        let err = create(src.to_str().unwrap(), &spec, &root, "").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);

        // directory gone but the branch still exists: still a conflict, with a clear reason.
        remove(&first.repo, &first.path).await.unwrap();
        let err = create(src.to_str().unwrap(), &spec, &root, "").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Conflict);
        assert!(err.message.contains("tessel/same-name"), "{}", err.message);
    }

    #[tokio::test]
    async fn clear_errors_for_bad_input() {
        let tmp = tempfile::tempdir().unwrap();
        let err = create(tmp.path().to_str().unwrap(), &WorktreeSpec::default(), &tmp.path().join("ws"), "x").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);
        assert!(err.message.contains("not inside a git repository"));

        let src = tmp.path().join("r");
        std::fs::create_dir_all(&src).unwrap();
        repo(&src).await;
        let bad_base = WorktreeSpec { base: Some("no-such-branch".into()), name: None };
        let err = create(src.to_str().unwrap(), &bad_base, &tmp.path().join("ws"), "x").await.unwrap_err();
        assert_eq!(err.code, ErrorCode::Invalid);

        let bad_name = WorktreeSpec { base: None, name: Some("!!!".into()) };
        assert_eq!(create(src.to_str().unwrap(), &bad_name, &tmp.path().join("ws"), "x").await.unwrap_err().code, ErrorCode::Invalid);
    }
}
