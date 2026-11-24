use std::path::Path;
use std::sync::Arc;
use tessel_agent::Backend;
use tessel_proto::ShipStep;

use crate::board::Land;
use crate::ship::git;

#[derive(Clone, Debug, PartialEq)]
pub enum Landed {
    Merged { sha: String, said: String },
    Opened { url: String, said: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Stuck {
    Conflict(Vec<String>),
    Checks(String),
    Other(String),
}

const LITTER: [&str; 8] = [":(exclude,glob)**/__pycache__/**", ":(exclude,glob)**/*.pyc", ":(exclude,glob)**/node_modules/**", ":(exclude,glob)**/.DS_Store", ":(exclude,glob)**/.pytest_cache/**", ":(exclude,glob)**/.mypy_cache/**", ":(exclude,glob)**/.ruff_cache/**", ":(exclude,glob)**/.tessel/settings.local.toml"];

fn typed(subject: &str) -> bool {
    regex::Regex::new(r"^[a-z]+(\([^)]*\))?!?: \S").is_ok_and(|re| re.is_match(subject)) && subject.chars().count() <= 72
}

async fn subject(backend: &Arc<dyn Backend>, ws: &Path, main: &str, title: &str) -> String {
    let subjects = git(ws, &["log", "--format=%s", &format!("{main}..HEAD")]).await.unwrap_or_default();
    let mine: Vec<&str> = subjects.lines().filter(|l| !l.trim().is_empty()).collect();
    if let [one] = mine.as_slice()
        && typed(one)
    {
        return (*one).to_owned();
    }
    let stat = git(ws, &["diff", "--stat", &format!("{main}...HEAD")]).await.unwrap_or_default();
    let prompt = format!(
        "Write one git commit subject for this whole change, in the style of these recent ones:\n{}\n\nThe task: {title}\nIts commits: {}\nFiles:\n{stat}\n\nAnswer with the subject only: lowercase after a type prefix like feat:, fix: or chore:, under 60 characters, no period.",
        git(ws, &["log", "--format=%s", "-n", "8", main]).await.unwrap_or_default(),
        mine.join("; ")
    );
    match backend.one_shot(prompt, "haiku").await {
        Ok(m) => m.lines().find(|l| !l.trim().is_empty()).unwrap_or(title).trim().trim_matches(['"', '`', '\'']).to_owned(),
        Err(_) => title.to_owned(),
    }
}

pub async fn land(backend: Arc<dyn Backend>, ws: &Path, root: &Path, title: &str, verify: &[String], mode: Land, pr: Option<&str>, issue: Option<&str>) -> Result<Landed, Stuck> {
    let main = crate::ship::main_branch(ws).await;
    let other = |e: String| Stuck::Other(e);
    let mut add = vec!["add", "-A", "--", "."];
    add.extend(LITTER);
    git(ws, &add).await.map_err(other)?;
    if !git(ws, &["diff", "--cached", "--name-only"]).await.map_err(other)?.is_empty() {
        let message = subject(&backend, ws, &main, title).await;
        git(ws, &["commit", "-q", "-m", &message]).await.map_err(other)?;
    }
    if git(ws, &["rebase", &main]).await.is_err() {
        let files: Vec<String> = git(ws, &["diff", "--name-only", "--diff-filter=U"]).await.unwrap_or_default().lines().map(str::to_owned).collect();
        let _ = git(ws, &["rebase", "--abort"]).await;
        return Err(Stuck::Conflict(files));
    }
    for command in verify {
        let v = crate::proof::run_check(ws, command, None).await;
        if !v.ok {
            return Err(Stuck::Checks(format!("{command}\n{}", v.output)));
        }
    }
    let message = subject(&backend, ws, &main, title).await;
    let closes = match issue {
        Some(url) => crate::issues::closes(root, url).await.map(|n| format!("Closes #{n}")),
        None => None,
    };
    match mode {
        Land::Main => {
            let here = git(root, &["rev-parse", "--abbrev-ref", "HEAD"]).await.map_err(other)?;
            if here != main {
                return Err(Stuck::Other(format!("the main checkout is on {here}, not {main}")));
            }
            if !git(root, &["status", "--porcelain", "--untracked-files=no"]).await.map_err(other)?.is_empty() {
                return Err(Stuck::Other("the main checkout has changes of its own".into()));
            }
            if git(ws, &["rev-list", "--count", &format!("{main}..HEAD")]).await.map_err(other)?.trim() == "0" {
                return Err(Stuck::Other("there's nothing to land".into()));
            }
            let branch = git(ws, &["rev-parse", "--abbrev-ref", "HEAD"]).await.map_err(other)?;
            git(ws, &["reset", "--soft", &main]).await.map_err(other)?;
            let whole = match &closes {
                Some(c) => format!("{message}\n\n{c}"),
                None => message.clone(),
            };
            git(ws, &["commit", "-q", "-m", &whole]).await.map_err(other)?;
            git(root, &["merge", "--ff-only", &branch]).await.map_err(other)?;
            let sha = git(root, &["rev-parse", "--short", "HEAD"]).await.unwrap_or_default();
            Ok(Landed::Merged { sha, said: format!("Merged into {main}.") })
        }
        Land::PullRequest if let Some(url) = pr => {
            let remote = crate::ship::home(root).await.remote;
            git(ws, &["push", "--force-with-lease", "-u", &remote, "HEAD"]).await.map_err(other)?;
            Ok(Landed::Opened { url: url.to_owned(), said: "Pushed the fix to its pull request.".into() })
        }
        Land::PullRequest => {
            let (said, url) = crate::ship::ship(ws, root, title, ShipStep::PullRequest { title: message, body: closes.unwrap_or_default(), draft: false }).await.map_err(other)?;
            Ok(Landed::Opened { url: url.unwrap_or_default(), said })
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

    fn repo() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let (root, ws) = (base.path().join("root"), base.path().join("ws"));
        std::fs::create_dir_all(&root).unwrap();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(&root, args);
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        std::fs::write(root.join("b.txt"), "b\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "feat: start"]);
        sh(&root, &["worktree", "add", "-q", "-b", "tessel/t1", ws.to_str().unwrap()]);
        (base, root, ws)
    }

    fn backend() -> Arc<dyn Backend> {
        Arc::new(tessel_agent::replay::ReplayBackend::new(vec![vec![]]))
    }

    #[tokio::test]
    async fn a_task_lands_as_one_commit_on_main_after_its_checks_pass_on_top_of_main() {
        let (_base, root, ws) = repo();
        std::fs::write(ws.join("a.txt"), "two\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "fix: count to two"]);
        std::fs::write(ws.join("c.txt"), "loose\n").unwrap();
        std::fs::create_dir_all(ws.join("__pycache__")).unwrap();
        std::fs::write(ws.join("__pycache__/a.cpython-39.pyc"), "junk").unwrap();
        std::fs::write(ws.join(".DS_Store"), "junk").unwrap();
        std::fs::write(root.join("b.txt"), "moved on\n").unwrap();
        sh(&root, &["commit", "-q", "-am", "chore: meanwhile"]);
        let landed = land(backend(), &ws, &root, "Count to two", &["test -f c.txt".into()], Land::Main, None, None).await.unwrap();
        let Landed::Merged { sha, .. } = landed else { panic!() };
        assert_eq!(sha, sh(&root, &["rev-parse", "--short", "HEAD"]));
        assert_eq!(sh(&root, &["log", "-1", "--format=%s"]), "fix: what the replay agent would say", "uncommitted work is committed, then all of it squashed");
        assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "two\n");
        assert_eq!(std::fs::read_to_string(root.join("b.txt")).unwrap(), "moved on\n", "on top of what main got meanwhile");
        let files = sh(&root, &["show", "--name-only", "--format=", "HEAD"]);
        assert!(files.contains("c.txt") && !files.contains("pycache") && !files.contains("DS_Store"), "build litter isn't landed: {files}");
    }

    #[tokio::test]
    async fn landing_an_issue_s_task_closes_that_issue_when_it_reaches_github_and_no_other() {
        let (_base, root, ws) = repo();
        sh(&root, &["remote", "add", "origin", "https://github.com/o/r.git"]);
        std::fs::write(ws.join("a.txt"), "two\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "fix: count to two"]);
        land(backend(), &ws, &root, "Count to two", &[], Land::Main, None, Some("https://github.com/o/r/issues/9")).await.unwrap();
        assert_eq!(sh(&root, &["log", "-1", "--format=%B"]), "fix: count to two\n\nCloses #9");
        assert!(!sh(&root, &["log", "-1", "--format=%B"]).to_lowercase().contains("co-authored"));
        std::fs::write(ws.join("b.txt"), "three\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "fix: three"]);
        land(backend(), &ws, &root, "Three", &[], Land::Main, None, Some("https://github.com/someone/else/issues/9")).await.unwrap();
        assert_eq!(sh(&root, &["log", "-1", "--format=%B"]), "fix: three", "another repository's issue isn't closed from here");
    }

    #[tokio::test]
    async fn a_conflict_or_a_failing_check_stops_it_and_says_why_and_main_is_untouched() {
        let (_base, root, ws) = repo();
        std::fs::write(ws.join("a.txt"), "mine\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "fix: mine"]);
        std::fs::write(root.join("a.txt"), "theirs\n").unwrap();
        sh(&root, &["commit", "-q", "-am", "fix: theirs"]);
        let before = sh(&root, &["rev-parse", "HEAD"]);
        assert_eq!(land(backend(), &ws, &root, "x", &[], Land::Main, None, None).await, Err(Stuck::Conflict(vec!["a.txt".into()])));
        assert_eq!(sh(&ws, &["status", "--porcelain"]), "", "the rebase was taken back");

        sh(&ws, &["reset", "-q", "--hard", "main"]);
        std::fs::write(ws.join("b.txt"), "new\n").unwrap();
        sh(&ws, &["commit", "-q", "-am", "fix: b"]);
        let stuck = land(backend(), &ws, &root, "x", &["echo 'error: nope' && false".into()], Land::Main, None, None).await.unwrap_err();
        assert!(matches!(&stuck, Stuck::Checks(out) if out.contains("error: nope")), "{stuck:?}");
        assert_eq!(sh(&root, &["rev-parse", "HEAD"]), before);
        assert_eq!(subject(&backend(), &ws, "main", "x").await, "fix: b", "a single typed commit keeps its own subject, for free");
    }
}
