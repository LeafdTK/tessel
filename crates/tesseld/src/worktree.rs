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
    git_with(dir, args, &[], timeout).await
}

async fn git_with(dir: &Path, args: &[&str], env: &[(&str, &str)], timeout: Duration) -> Result<String, String> {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").envs(env.iter().copied()).stdin(std::process::Stdio::null()).kill_on_drop(true);
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
    if let Ok(start) = git(&repo_path, &["rev-parse", &format!("{base}^{{commit}}")], QUICK).await {
        let _ = git(&repo_path, &["update-ref", &base_ref(&branch), start.trim()], QUICK).await;
    }
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
pub fn shared_build(ws: &Path, root: &Path) -> Vec<(String, String)> {
    let same = |a: &Path, b: &Path| std::fs::canonicalize(a).ok().zip(std::fs::canonicalize(b).ok()).is_some_and(|(a, b)| a == b);
    if same(ws, root) || !root.join("Cargo.toml").is_file() || std::env::var_os("CARGO_TARGET_DIR").is_some_and(|v| !v.is_empty()) {
        return Vec::new();
    }
    let (t, _) = crate::setup::merged(ws, root);
    if t.get("factory").and_then(|f| f.get("share_build")).and_then(toml::Value::as_bool) == Some(false) {
        return Vec::new();
    }
    vec![("CARGO_TARGET_DIR".into(), root.join("target").to_string_lossy().into_owned())]
}

pub fn base_ref(branch: &str) -> String {
    format!("refs/tessel/base/{branch}")
}

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

pub fn kept_ref(session: &str) -> String {
    format!("refs/tessel/archive/{session}")
}

const AS_TESSEL: [(&str, &str); 4] = [("GIT_AUTHOR_NAME", "tessel"), ("GIT_AUTHOR_EMAIL", "tessel@localhost"), ("GIT_COMMITTER_NAME", "tessel"), ("GIT_COMMITTER_EMAIL", "tessel@localhost")];

pub async fn keep_work(path: &str, session: &str) -> Option<String> {
    let dir = Path::new(path);
    let head = git(dir, &["rev-parse", "HEAD"], QUICK).await.ok()?;
    let index = std::env::temp_dir().join(format!("tessel-keep-{session}"));
    let index_path = index.to_string_lossy().into_owned();
    let apart = [("GIT_INDEX_FILE", index_path.as_str())];
    let kept = async {
        git_with(dir, &["read-tree", "HEAD"], &apart, QUICK).await?;
        git_with(dir, &["add", "-A"], &apart, Duration::from_secs(120)).await?;
        let tree = git_with(dir, &["write-tree"], &apart, QUICK).await?;
        if tree == git(dir, &["rev-parse", "HEAD^{tree}"], QUICK).await? {
            return Ok(None);
        }
        let commit = git_with(dir, &["commit-tree", &tree, "-p", &head, "-m", "tessel: work kept when its chat was deleted"], &AS_TESSEL, QUICK).await?;
        git(dir, &["update-ref", &kept_ref(session), &commit], QUICK).await?;
        Ok::<_, String>(Some(commit))
    }
    .await;
    let _ = std::fs::remove_file(&index);
    match kept {
        Ok(k) => k,
        Err(e) => {
            eprintln!("tesseld: keeping the work in {path}: {e}");
            None
        }
    }
}

pub async fn bring_back(repo: &str, path: &str, branch: &str, kept: Option<&str>) -> Result<(), String> {
    let repo_dir = Path::new(repo);
    if Path::new(path).exists() {
        return Ok(());
    }
    {
        let _ = git(repo_dir, &["worktree", "prune"], QUICK).await;
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("creating {}: {e}", parent.display()))?;
        }
        if let Err(e) = git(repo_dir, &["worktree", "add", path, branch], Duration::from_secs(120)).await {
            let start = kept.map(|k| format!("{k}^")).ok_or(e)?;
            git(repo_dir, &["worktree", "add", "-b", branch, path, &start], Duration::from_secs(120)).await?;
        }
    }
    if let Some(k) = kept {
        git(Path::new(path), &["restore", "--source", k, "--worktree", "--", "."], Duration::from_secs(120)).await?;
    }
    Ok(())
}

pub async fn forget_work(repo: &str, session: &str) {
    let _ = git(Path::new(repo), &["update-ref", "-d", &kept_ref(session)], QUICK).await;
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

    #[tokio::test]
    async fn work_kept_when_a_workspace_goes_comes_back_with_it() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("r");
        std::fs::create_dir_all(&src).unwrap();
        repo(&src).await;
        std::fs::write(src.join("a.txt"), "one\n").unwrap();
        git(&src, &["add", "a.txt"], QUICK).await.unwrap();
        git(&src, &["commit", "-q", "-m", "a"], QUICK).await.unwrap();
        let made = create(src.to_str().unwrap(), &WorktreeSpec::default(), &tmp.path().join("ws"), "keep").await.unwrap();
        let ws = Path::new(&made.path);

        assert_eq!(keep_work(&made.path, "s1").await, None, "nothing changed, nothing to keep");

        std::fs::write(ws.join("a.txt"), "two\n").unwrap();
        std::fs::write(ws.join("new.txt"), "fresh\n").unwrap();
        git(ws, &["add", "a.txt"], QUICK).await.unwrap();
        let staged_before = git(ws, &["diff", "--cached", "--name-only"], QUICK).await.unwrap();
        let kept = keep_work(&made.path, "s1").await.expect("the work is kept");
        assert_eq!(git(ws, &["diff", "--cached", "--name-only"], QUICK).await.unwrap(), staged_before, "the real index is left alone");
        assert_eq!(git(&src, &["stash", "list"], QUICK).await.unwrap(), "", "and so is the stash");

        remove(&made.repo, &made.path).await.unwrap();
        assert!(!ws.exists());

        bring_back(&made.repo, &made.path, &made.branch, Some(&kept)).await.unwrap();
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(ws.join("new.txt")).unwrap(), "fresh\n");
        assert_eq!(git(ws, &["rev-parse", "--abbrev-ref", "HEAD"], QUICK).await.unwrap(), made.branch);

        forget_work(&made.repo, "s1").await;
        assert!(git(&src, &["show-ref", "--verify", "--quiet", &kept_ref("s1")], QUICK).await.is_err());
    }
}
