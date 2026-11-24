//! getting a workspace's work into the main branch: by pull request, or
//! straight in for those who don't use them.
//!
//! nothing here makes a merge commit: a pull request is squashed or rebased,
//! and merging straight in rebases the branch onto main and moves main forward
//! to it. a step that would leave a mess stops before making one: a rebase that
//! conflicts is taken back and the conflicting files named, so they can be
//! handed to the agent; a main checkout with work of its own in it is left alone.

use std::path::Path;
use std::time::Duration;
use tessel_proto::ShipStep;

async fn run(program: &str, dir: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new(program);
    cmd.current_dir(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1").stdin(std::process::Stdio::null()).kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(secs), cmd.output())
        .await
        .map_err(|_| format!("{program} {} took too long", args.first().unwrap_or(&"")))?
        .map_err(|e| format!("couldn't run {program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        Err(if err.is_empty() { format!("{program} {} failed", args.join(" ")) } else { err })
    }
}

pub(crate) async fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    run("git", dir, args, 60).await
}

/// the repository's main branch: what `origin` points at, else main, else master.
#[derive(Clone, Debug, PartialEq)]
pub struct Home {
    pub remote: String,
    pub branch: String,
    pub upstream: String,
}

async fn tracked(dir: &Path) -> Option<Home> {
    let common = std::path::PathBuf::from(git(dir, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await.ok()?);
    let root = if common.file_name().is_some_and(|n| n == ".git") { common.parent()?.to_path_buf() } else { return None };
    let branch = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]).await.ok()?;
    let upstream = git(&root, &["rev-parse", "--abbrev-ref", &format!("{branch}@{{upstream}}")]).await.ok()?;
    let remote = upstream.split_once('/')?.0.to_owned();
    let head = git(&root, &["symbolic-ref", "--short", &format!("refs/remotes/{remote}/HEAD")]).await.ok()?;
    (head == upstream).then_some(Home { remote, branch, upstream })
}

pub async fn home(dir: &Path) -> Home {
    match tracked(dir).await {
        Some(h) => h,
        None => {
            let branch = main_branch(dir).await;
            Home { remote: "origin".into(), upstream: format!("origin/{branch}"), branch }
        }
    }
}

pub async fn main_branch(dir: &Path) -> String {
    if let Some(h) = tracked(dir).await {
        return h.branch;
    }
    if let Ok(head) = git(dir, &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"]).await {
        return head.trim_start_matches("origin/").to_owned();
    }
    for b in ["main", "master"] {
        if git(dir, &["show-ref", "--verify", "--quiet", &format!("refs/heads/{b}")]).await.is_ok() {
            return b.to_owned();
        }
    }
    "main".to_owned()
}

pub fn github_repo(url: &str) -> Option<String> {
    let rest = url.trim().split_once("github.com").map(|(_, r)| r)?;
    let path = rest.trim_start_matches([':', '/']).trim_end_matches('/').trim_end_matches(".git");
    let mut parts = path.split('/');
    let (owner, name) = (parts.next()?, parts.next()?);
    (!owner.is_empty() && !name.is_empty() && parts.next().is_none()).then(|| format!("{owner}/{name}"))
}

/// a rebase that stopped: take it back, and say which files clashed.
async fn conflicted(dir: &Path) -> String {
    let files = git(dir, &["diff", "--name-only", "--diff-filter=U"]).await.unwrap_or_default();
    let _ = git(dir, &["rebase", "--abort"]).await;
    let files: Vec<&str> = files.lines().filter(|l| !l.is_empty()).collect();
    format!("Conflicts in {}. Nothing was changed.", if files.is_empty() { "some files".to_owned() } else { files.join(", ") })
}

/// do one step. what it did, in words, and a link if it made one.
pub async fn ship(ws: &Path, root: &Path, goal: &str, step: ShipStep) -> Result<(String, Option<String>), String> {
    let Home { remote, branch: main, upstream } = home(root).await;
    let branch = git(ws, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
    match step {
        ShipStep::Push => {
            run("git", ws, &["push", "-u", &remote, "HEAD"], 120).await?;
            Ok((format!("Pushed {branch}."), None))
        }
        ShipStep::PullRequest { title, body, draft } => {
            run("git", ws, &["push", "-u", &remote, "HEAD"], 120).await?;
            let title = if title.trim().is_empty() { goal.to_owned() } else { title };
            let repo = github_repo(&git(ws, &["remote", "get-url", &remote]).await.unwrap_or_default());
            let base = upstream.split_once('/').map_or(main.as_str(), |(_, b)| b).to_owned();
            let mut args = vec!["pr", "create", "--head", &branch, "--base", &base, "--title", &title, "--body", &body];
            if let Some(r) = &repo {
                args.extend(["--repo", r.as_str()]);
            }
            if draft {
                args.push("--draft");
            }
            let out = run("gh", ws, &args, 60).await?;
            let url = out.lines().rev().find(|l| l.starts_with("http")).map(str::to_owned);
            Ok((format!("Opened a pull request into {main}."), url))
        }
        ShipStep::MergePullRequest { squash } => {
            let mut args = vec!["pr", "merge", branch.as_str(), if squash { "--squash" } else { "--rebase" }];
            if squash {
                args.extend(["--body", ""]);
            }
            run("gh", ws, &args, 120).await?;
            Ok((format!("Merged into {main}, {}.", if squash { "squashed" } else { "rebased" }), None))
        }
        ShipStep::Update => {
            run("git", ws, &["fetch", &remote], 120).await?;
            if git(ws, &["rebase", "--autostash", &upstream]).await.is_err() {
                return Err(conflicted(ws).await);
            }
            Ok((format!("Up to date with {main}."), None))
        }
        ShipStep::MergeIntoMain { squash, message, push } => {
            let here = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await?;
            if here != main {
                return Err(format!("The main checkout is on {here}, not {main}. Switch it to {main} first."));
            }
            if !git(root, &["status", "--porcelain", "--untracked-files=no"]).await?.is_empty() {
                return Err("The main checkout has changes of its own. Commit or stash them first.".into());
            }
            if !git(ws, &["status", "--porcelain"]).await?.is_empty() {
                return Err("This workspace has uncommitted changes. Commit them first.".into());
            }
            if git(ws, &["rebase", &main]).await.is_err() {
                return Err(conflicted(ws).await);
            }
            let ahead = git(ws, &["rev-list", "--count", &format!("{main}..HEAD")]).await?;
            if ahead.trim() == "0" {
                return Err(format!("Nothing to merge: {branch} has nothing {main} doesn't."));
            }
            if squash {
                let message = if message.trim().is_empty() { goal.to_owned() } else { message };
                git(ws, &["reset", "--soft", &main]).await?;
                git(ws, &["commit", "-q", "-m", &message]).await?;
            }
            git(root, &["merge", "--ff-only", &branch]).await?;
            if push {
                let to = format!("{main}:{}", upstream.split_once('/').map_or(main.as_str(), |(_, b)| b));
                run("git", root, &["push", &remote, &to], 120).await?;
            }
            Ok((format!("Merged into {main}{}.", if push { " and pushed" } else { "" }), None))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sh(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// a repository on main with a remote, and a worktree on a branch of it.
    fn repo(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("tessel-ship-{name}-{}", uuid::Uuid::new_v4()));
        let (origin, root, ws) = (base.join("origin.git"), base.join("root"), base.join("ws"));
        std::fs::create_dir_all(&root).unwrap();
        sh(&base, &["init", "-q", "--bare", "-b", "main", origin.to_str().unwrap()]);
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(&root, args);
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "first"]);
        sh(&root, &["remote", "add", "origin", origin.to_str().unwrap()]);
        sh(&root, &["push", "-q", "-u", "origin", "main"]);
        sh(&root, &["worktree", "add", "-q", "-b", "tessel/x", ws.to_str().unwrap()]);
        (base, root, ws)
    }

    #[tokio::test]
    async fn a_checkout_that_tracks_another_remote_s_default_branch_lands_and_pushes_there_not_to_origin() {
        let (base, root, ws) = repo("two-remotes");
        let (old, new) = (base.join("old.git"), base.join("new.git"));
        sh(&base, &["init", "-q", "--bare", "-b", "master", old.to_str().unwrap()]);
        sh(&base, &["init", "-q", "--bare", "-b", "main", new.to_str().unwrap()]);
        sh(&root, &["remote", "remove", "origin"]);
        sh(&root, &["remote", "add", "origin", old.to_str().unwrap()]);
        sh(&root, &["push", "-q", "origin", "main:master"]);
        sh(&root, &["remote", "set-head", "origin", "master"]);
        sh(&root, &["remote", "add", "tessel", new.to_str().unwrap()]);
        sh(&root, &["push", "-q", "-u", "tessel", "main"]);
        sh(&root, &["remote", "set-head", "tessel", "main"]);
        assert_eq!(main_branch(&ws).await, "main");
        assert_eq!(home(&ws).await, Home { remote: "tessel".into(), branch: "main".into(), upstream: "tessel/main".into() });
        std::fs::write(ws.join("a.txt"), "two\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "step"]);
        ship(&ws, &root, "Two", ShipStep::MergeIntoMain { squash: true, message: String::new(), push: true }).await.unwrap();
        assert_eq!(sh(&base, &["--git-dir", new.to_str().unwrap(), "log", "-1", "--format=%s", "main"]), "Two", "pushed where main comes from");
        assert_eq!(sh(&base, &["--git-dir", old.to_str().unwrap(), "log", "-1", "--format=%s", "master"]), "first", "origin is left alone");
        sh(&root, &["checkout", "-q", "-b", "side"]);
        assert_eq!(home(&ws).await.remote, "origin", "a checkout on some other branch falls back to origin's idea");
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn a_github_remote_names_its_repository() {
        assert_eq!(github_repo("https://github.com/LeafdTK/tessel.git").as_deref(), Some("LeafdTK/tessel"));
        assert_eq!(github_repo("git@github.com:o/r.git").as_deref(), Some("o/r"));
        assert_eq!(github_repo("/tmp/r.git"), None);
    }

    #[tokio::test]
    async fn straight_into_main_squashes_moves_main_forward_and_can_push() {
        let (base, root, ws) = repo("merge");
        for (n, text) in [(1, "two\n"), (2, "three\n")] {
            std::fs::write(ws.join("a.txt"), text).unwrap();
            sh(&ws, &["commit", "-q", "-am", &format!("step {n}")]);
        }
        let (said, _) = ship(&ws, &root, "Count higher", ShipStep::MergeIntoMain { squash: true, message: String::new(), push: true }).await.unwrap();
        assert_eq!(said, "Merged into main and pushed.");
        assert_eq!(sh(&root, &["log", "-1", "--format=%s"]), "Count higher", "one commit, named for the work");
        assert_eq!(sh(&root, &["rev-list", "--count", "HEAD"]), "2");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "three\n", "the main checkout has it");
        assert_eq!(sh(&root, &["rev-parse", "origin/main"]), sh(&root, &["rev-parse", "main"]), "and so does origin");
        assert!(ship(&ws, &root, "", ShipStep::MergeIntoMain { squash: true, message: String::new(), push: false }).await.unwrap_err().contains("Nothing to merge"));
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn a_main_checkout_with_its_own_work_is_left_alone() {
        let (base, root, ws) = repo("dirty");
        std::fs::write(ws.join("a.txt"), "two\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "two"]);
        std::fs::write(root.join("a.txt"), "mine\n").unwrap();
        let e = ship(&ws, &root, "x", ShipStep::MergeIntoMain { squash: false, message: String::new(), push: false }).await.unwrap_err();
        assert!(e.contains("changes of its own"), "{e}");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "mine\n");
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn an_update_that_conflicts_changes_nothing_and_names_the_files() {
        let (base, root, ws) = repo("update");
        std::fs::write(ws.join("a.txt"), "branch\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "branch"]);
        let before = sh(&ws, &["rev-parse", "HEAD"]);
        std::fs::write(root.join("a.txt"), "main\n").unwrap();
        sh(&root, &["commit", "-q", "-am", "main"]);
        sh(&root, &["push", "-q", "origin", "main"]);
        let e = ship(&ws, &root, "x", ShipStep::Update).await.unwrap_err();
        assert!(e.starts_with("Conflicts in a.txt"), "{e}");
        assert_eq!(sh(&ws, &["rev-parse", "HEAD"]), before, "the branch as it was");
        assert!(sh(&ws, &["status", "--porcelain"]).is_empty(), "and no half-done rebase");
        // without a clash, it's replayed on top.
        std::fs::write(ws.join("b.txt"), "new\n").unwrap();
        sh(&ws, &["reset", "-q", "--hard", "origin/main"]);
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-q", "-m", "b"]);
        assert_eq!(ship(&ws, &root, "x", ShipStep::Update).await.unwrap().0, "Up to date with main.");
        let _ = std::fs::remove_dir_all(base);
    }
}
