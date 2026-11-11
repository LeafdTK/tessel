//! a session's working directory: what changed in it, what is in it, and
//! where its pull request stands.
//!
//! everything here runs on the daemon's machine, on behalf of a client that
//! may be somewhere else. two rules follow from that:
//! - a path from a client is relative to the session's directory and can
//!   never be made to point outside it.
//! - nothing blocks for long: every external command has a time limit.

use photon_proto::{
    ChangeStatus, Check, CheckState, DirEntry, ErrorCode, FileChange, FileContent, FileDiff, GitSummary, PrStatus,
    PrSummary, RpcError,
};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;
use tokio::process::Command;

/// the largest file sent whole. anything bigger is cut and marked truncated.
pub const MAX_FILE: usize = 1024 * 1024;

const QUICK: Duration = Duration::from_secs(10);

fn invalid(msg: impl Into<String>) -> RpcError {
    RpcError::new(ErrorCode::Invalid, msg)
}

/// turn a client's relative path into a real one inside `root`, or refuse.
///
/// refuses absolute paths and anything that climbs out with `..`. then, if
/// the target (or, for a file about to be created, its parent) exists, checks
/// where it *really* is, so a symlink inside the directory can't be used to
/// reach outside it.
pub fn resolve(root: &Path, rel: &str) -> Result<PathBuf, RpcError> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() {
        return Err(invalid("paths are relative to the session's folder"));
    }
    let mut clean = PathBuf::new();
    for part in rel_path.components() {
        match part {
            Component::Normal(p) => clean.push(p),
            Component::CurDir => {}
            _ => return Err(invalid(format!("{rel} is outside the session's folder"))),
        }
    }
    let root = root.canonicalize().map_err(|e| RpcError::new(ErrorCode::Conflict, format!("the session's folder is gone: {e}")))?;
    let full = root.join(&clean);
    // check the deepest part that exists.
    let mut probe = full.clone();
    let real = loop {
        match probe.canonicalize() {
            Ok(real) => break real,
            Err(_) => {
                if !probe.pop() {
                    break root.clone();
                }
            }
        }
    };
    if !real.starts_with(&root) {
        return Err(invalid(format!("{rel} is outside the session's folder")));
    }
    Ok(full)
}

struct Output {
    ok: bool,
    code: Option<i32>,
    stdout: Vec<u8>,
    stderr: String,
}

async fn run(program: &str, dir: &Path, args: &[&str], limit: Duration) -> Result<Output, String> {
    let mut cmd = Command::new(program);
    cmd.current_dir(dir).args(args).env("GIT_TERMINAL_PROMPT", "0").env("GH_PROMPT_DISABLED", "1").stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(limit, cmd.output()).await {
        Err(_) => Err(format!("{program} {} took too long", args.first().unwrap_or(&""))),
        Ok(Err(e)) => Err(format!("could not run {program}: {e}")),
        Ok(Ok(out)) => Ok(Output { ok: out.status.success(), code: out.status.code(), stdout: out.stdout, stderr: String::from_utf8_lossy(&out.stderr).trim().to_owned() }),
    }
}

async fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = run("git", dir, args, QUICK).await.ok()?;
    out.ok.then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// the commit this work is measured from, and what to call it.
///
/// the point where the branch left the repository's main line if we can find
/// one, so that committed work counts as a change too. otherwise the last
/// commit, which shows uncommitted work only.
async fn base(dir: &Path) -> (Option<String>, Option<String>) {
    let head = git(dir, &["rev-parse", "--verify", "-q", "HEAD"]).await;
    if head.is_none() {
        return (None, None);
    }
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await.unwrap_or_default();
    let mut candidates: Vec<String> = Vec::new();
    if let Some(remote_head) = git(dir, &["symbolic-ref", "--short", "-q", "refs/remotes/origin/HEAD"]).await.filter(|s| !s.is_empty()) {
        candidates.push(remote_head);
    }
    candidates.extend(["origin/main", "origin/master", "main", "master"].map(str::to_owned));
    for name in candidates {
        if name == branch {
            continue;
        }
        if let Some(fork) = git(dir, &["merge-base", "HEAD", &name]).await.filter(|s| !s.is_empty()) {
            return (Some(name), Some(fork));
        }
    }
    (None, head)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.iter().take(8000).any(|b| *b == 0)
}

fn count_lines(bytes: &[u8]) -> u32 {
    if bytes.is_empty() {
        return 0;
    }
    let newlines = bytes.iter().filter(|b| **b == b'\n').count() as u32;
    if bytes.last() == Some(&b'\n') { newlines } else { newlines + 1 }
}

/// everything that differs from where this work started, committed or not.
pub async fn git_status(dir: &Path) -> GitSummary {
    if git(dir, &["rev-parse", "--is-inside-work-tree"]).await.as_deref() != Some("true") {
        return GitSummary::default();
    }
    let branch = git(dir, &["rev-parse", "--abbrev-ref", "HEAD"]).await.filter(|b| !b.is_empty() && b != "HEAD");
    let (base_name, base_commit) = base(dir).await;
    let mut files: Vec<FileChange> = Vec::new();

    if let Some(commit) = &base_commit {
        // -z: paths are NUL separated and never quoted, so any filename survives.
        let status = git_bytes(dir, &["diff", "--name-status", "--no-renames", "-z", commit]).await;
        let numstat = git_bytes(dir, &["diff", "--numstat", "--no-renames", "-z", commit]).await;
        let mut counts = std::collections::HashMap::new();
        for rec in numstat.split(|b| *b == 0).filter(|r| !r.is_empty()) {
            let text = String::from_utf8_lossy(rec);
            let mut parts = text.splitn(3, '\t');
            if let (Some(a), Some(r), Some(path)) = (parts.next(), parts.next(), parts.next()) {
                // binary files report "-" for both counts.
                counts.insert(path.to_owned(), (a.parse::<u32>().ok(), r.parse::<u32>().ok()));
            }
        }
        let fields: Vec<&[u8]> = status.split(|b| *b == 0).collect();
        for pair in fields.chunks(2) {
            let [code, path] = pair else { continue };
            if path.is_empty() {
                continue;
            }
            let path = String::from_utf8_lossy(path).into_owned();
            let status = match code.first() {
                Some(b'A') => ChangeStatus::Added,
                Some(b'D') => ChangeStatus::Deleted,
                Some(b'M') | Some(b'T') => ChangeStatus::Modified,
                _ => ChangeStatus::Unknown,
            };
            let (added, removed) = counts.get(&path).copied().unwrap_or((Some(0), Some(0)));
            files.push(FileChange { path, status, added: added.unwrap_or(0), removed: removed.unwrap_or(0), binary: added.is_none() });
        }
    }

    // files git has never been told about. in a repository with no commits yet, that is every file.
    let mut listing = git_bytes(dir, &["ls-files", "--others", "--exclude-standard", "-z"]).await;
    if base_commit.is_none() {
        listing.extend(git_bytes(dir, &["ls-files", "-z"]).await);
    }
    for path in listing.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        let path = String::from_utf8_lossy(path).into_owned();
        if files.iter().any(|f| f.path == path) {
            continue;
        }
        let bytes = read_capped(&dir.join(&path)).await.map(|(b, _)| b).unwrap_or_default();
        let binary = is_binary(&bytes);
        files.push(FileChange { path, status: ChangeStatus::Untracked, added: if binary { 0 } else { count_lines(&bytes) }, removed: 0, binary });
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    let ahead = match &base_name {
        Some(name) => git(dir, &["rev-list", "--count", &format!("{name}..HEAD")]).await.and_then(|n| n.parse().ok()).unwrap_or(0),
        None => 0,
    };
    GitSummary {
        is_repo: true,
        branch,
        base: base_name,
        ahead,
        added: files.iter().map(|f| f.added).sum(),
        removed: files.iter().map(|f| f.removed).sum(),
        files,
    }
}

async fn git_bytes(dir: &Path, args: &[&str]) -> Vec<u8> {
    match run("git", dir, args, QUICK).await {
        Ok(out) if out.ok => out.stdout,
        _ => Vec::new(),
    }
}

/// read a file, up to the size limit. returns the bytes and whether it was cut.
async fn read_capped(path: &Path) -> Option<(Vec<u8>, bool)> {
    use tokio::io::AsyncReadExt;
    let file = tokio::fs::File::open(path).await.ok()?;
    let mut buf = Vec::new();
    file.take(MAX_FILE as u64 + 1).read_to_end(&mut buf).await.ok()?;
    let cut = buf.len() > MAX_FILE;
    buf.truncate(MAX_FILE);
    Some((buf, cut))
}

fn text_of(bytes: Vec<u8>, cut: bool) -> String {
    let mut bytes = bytes;
    if cut {
        // don't end in the middle of a character.
        while !bytes.is_empty() && std::str::from_utf8(&bytes).is_err() {
            bytes.pop();
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// one file as it was where the work started and as it is now.
pub async fn git_diff(dir: &Path, rel: &str) -> Result<FileDiff, RpcError> {
    let full = resolve(dir, rel)?;
    let (_, base_commit) = base(dir).await;
    let mut diff = FileDiff { path: rel.to_owned(), ..Default::default() };

    let old_bytes = match &base_commit {
        Some(commit) => match run("git", dir, &["show", &format!("{commit}:{rel}")], QUICK).await {
            Ok(out) if out.ok => Some(out.stdout),
            _ => None,
        },
        None => None,
    };
    let new = read_capped(&full).await;
    if old_bytes.is_none() && new.is_none() {
        return Err(RpcError::new(ErrorCode::NotFound, format!("{rel} is not in this session's folder or its history")));
    }
    let binary = old_bytes.as_deref().is_some_and(is_binary) || new.as_ref().is_some_and(|(b, _)| is_binary(b));
    diff.binary = binary;
    if binary {
        return Ok(diff);
    }
    if let Some(mut old) = old_bytes {
        let cut = old.len() > MAX_FILE;
        old.truncate(MAX_FILE);
        diff.truncated |= cut;
        diff.old = Some(text_of(old, cut));
    }
    if let Some((bytes, cut)) = new {
        diff.truncated |= cut;
        diff.new = Some(text_of(bytes, cut));
    }
    // the same change as a patch. a file git doesn't know is compared with nothing.
    let unified = match (&base_commit, diff.old.is_some()) {
        (Some(commit), true) => run("git", dir, &["diff", "--no-renames", commit, "--", rel], QUICK).await,
        _ => run("git", dir, &["diff", "--no-index", "--", "/dev/null", rel], QUICK).await,
    };
    if let Ok(out) = unified {
        // `--no-index` exits 1 when the files differ, which is the expected case.
        if out.ok || out.code == Some(1) {
            let cut = out.stdout.len() > MAX_FILE;
            let mut bytes = out.stdout;
            bytes.truncate(MAX_FILE);
            diff.truncated |= cut;
            diff.unified = text_of(bytes, cut);
        }
    }
    Ok(diff)
}

/// what is in a directory: folders first, then files, each by name. `.git` is left out.
pub async fn list(dir: &Path, rel: &str) -> Result<Vec<DirEntry>, RpcError> {
    let full = resolve(dir, rel)?;
    let mut read = tokio::fs::read_dir(&full).await.map_err(|e| RpcError::new(ErrorCode::NotFound, format!("can't open {}: {e}", if rel.is_empty() { "the session's folder" } else { rel })))?;
    let mut entries = Vec::new();
    while let Ok(Some(entry)) = read.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".git" {
            continue;
        }
        // follow links, so a linked folder opens like a folder.
        let Ok(meta) = tokio::fs::metadata(entry.path()).await else { continue };
        entries.push(DirEntry { name, is_dir: meta.is_dir(), size: if meta.is_dir() { 0 } else { meta.len() } });
    }
    entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    Ok(entries)
}

pub async fn read(dir: &Path, rel: &str) -> Result<FileContent, RpcError> {
    let full = resolve(dir, rel)?;
    let meta = tokio::fs::metadata(&full).await.map_err(|e| RpcError::new(ErrorCode::NotFound, format!("can't open {rel}: {e}")))?;
    if meta.is_dir() {
        return Err(invalid(format!("{rel} is a folder")));
    }
    let (bytes, cut) = read_capped(&full).await.ok_or_else(|| RpcError::new(ErrorCode::NotFound, format!("can't read {rel}")))?;
    let binary = is_binary(&bytes);
    Ok(FileContent { path: rel.to_owned(), text: (!binary).then(|| text_of(bytes, cut)), binary, truncated: cut, size: meta.len() })
}

/// replace a file's contents. the write is all or nothing: a crash part way
/// through leaves the old file, never half of the new one.
pub async fn write(dir: &Path, rel: &str, content: &str) -> Result<(), RpcError> {
    let full = resolve(dir, rel)?;
    if full.is_dir() {
        return Err(invalid(format!("{rel} is a folder")));
    }
    let parent = full.parent().ok_or_else(|| invalid("no such folder"))?;
    if !parent.is_dir() {
        return Err(RpcError::new(ErrorCode::NotFound, format!("the folder for {rel} does not exist")));
    }
    let name = full.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{name}.photon-{}", uuid::Uuid::new_v4().simple()));
    let internal = |e: std::io::Error| RpcError::new(ErrorCode::Internal, format!("could not save {rel}: {e}"));
    tokio::fs::write(&tmp, content).await.map_err(internal)?;
    // keep the file's permissions (an executable script stays executable).
    if let Ok(meta) = tokio::fs::metadata(&full).await {
        let _ = tokio::fs::set_permissions(&tmp, meta.permissions()).await;
    }
    if let Err(e) = tokio::fs::rename(&tmp, &full).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(internal(e));
    }
    Ok(())
}

/// the pull request for the current branch, read through the github cli.
pub async fn pr_status(dir: &Path) -> PrStatus {
    const FIELDS: &str = "number,title,url,state,isDraft,baseRefName,headRefName,statusCheckRollup";
    let out = match run("gh", dir, &["pr", "view", "--json", FIELDS], Duration::from_secs(15)).await {
        Ok(out) => out,
        Err(e) if e.contains("could not run") => return PrStatus { pr: None, note: Some("Install the GitHub CLI (gh) to see pull requests here.".into()) },
        Err(e) => return PrStatus { pr: None, note: Some(e) },
    };
    if !out.ok {
        let note = if out.stderr.contains("no pull requests found") {
            "No pull request for this branch yet.".to_owned()
        } else if out.stderr.contains("gh auth login") || out.stderr.contains("not logged") {
            "Sign in to the GitHub CLI (gh auth login) to see pull requests here.".to_owned()
        } else if out.stderr.contains("not a git repository") || out.stderr.contains("no git remotes") {
            "This folder has no GitHub remote.".to_owned()
        } else {
            out.stderr.lines().next().unwrap_or("Could not read the pull request.").to_owned()
        };
        return PrStatus { pr: None, note: Some(note) };
    }
    match parse_pr(&out.stdout) {
        Some(pr) => PrStatus { pr: Some(pr), note: None },
        None => PrStatus { pr: None, note: Some("Could not read the pull request.".into()) },
    }
}

/// read `gh pr view --json` output. tolerant: a missing field is a default, not a failure.
pub fn parse_pr(json: &[u8]) -> Option<PrSummary> {
    let v: serde_json::Value = serde_json::from_slice(json).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_owned();
    let checks = v
        .get("statusCheckRollup")
        .and_then(|c| c.as_array())
        .into_iter()
        .flatten()
        .map(|c| {
            let g = |k: &str| c.get(k).and_then(|x| x.as_str()).unwrap_or_default().to_uppercase();
            // two shapes: check runs (status + conclusion) and commit statuses (state).
            let (status, conclusion, state) = (g("status"), g("conclusion"), g("state"));
            let outcome = if !conclusion.is_empty() { conclusion } else { state };
            let state = match outcome.as_str() {
                "SUCCESS" | "NEUTRAL" => CheckState::Passing,
                "FAILURE" | "ERROR" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "STARTUP_FAILURE" => CheckState::Failing,
                "SKIPPED" => CheckState::Skipped,
                "PENDING" | "EXPECTED" | "" if status != "COMPLETED" => CheckState::Pending,
                _ => CheckState::Unknown,
            };
            let name = ["name", "context", "workflowName"].iter().find_map(|k| c.get(*k).and_then(|x| x.as_str()).filter(|n| !n.is_empty())).unwrap_or("check").to_owned();
            let url = ["detailsUrl", "targetUrl"].iter().find_map(|k| c.get(*k).and_then(|x| x.as_str()).filter(|u| !u.is_empty())).map(str::to_owned);
            Check { name, state, url }
        })
        .collect();
    Some(PrSummary {
        number: v.get("number")?.as_u64()?,
        title: s("title"),
        url: s("url"),
        state: s("state").to_lowercase(),
        draft: v.get("isDraft").and_then(|d| d.as_bool()).unwrap_or(false),
        base: s("baseRefName"),
        head: s("headRefName"),
        checks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn sh(dir: &Path, args: &[&str]) {
        let out = run("git", dir, args, QUICK).await.unwrap();
        assert!(out.ok, "git {args:?}: {}", out.stderr);
    }

    /// a repository with one commit on `main`, then a `work` branch checked out.
    async fn repo() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        let d = tmp.path();
        for args in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(d, args).await;
        }
        std::fs::write(d.join("keep.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(d.join("gone.txt"), "bye\n").unwrap();
        std::fs::create_dir(d.join("src")).unwrap();
        std::fs::write(d.join("src/lib.rs"), "fn a() {}\n").unwrap();
        std::fs::write(d.join(".gitignore"), "target/\n").unwrap();
        sh(d, &["add", "-A"]).await;
        sh(d, &["commit", "-q", "-m", "init"]).await;
        sh(d, &["checkout", "-q", "-b", "work"]).await;
        tmp
    }

    fn change<'a>(s: &'a GitSummary, path: &str) -> &'a FileChange {
        s.files.iter().find(|f| f.path == path).unwrap_or_else(|| panic!("{path} not in {:?}", s.files))
    }

    #[tokio::test]
    async fn status_counts_committed_and_uncommitted_work_against_the_main_line() {
        let tmp = repo().await;
        let d = tmp.path();
        // committed on the branch:
        std::fs::write(d.join("src/lib.rs"), "fn a() {}\nfn b() {}\n").unwrap();
        sh(d, &["commit", "-q", "-am", "add b"]).await;
        // not committed:
        std::fs::write(d.join("keep.txt"), "one\n2\nthree\nfour\n").unwrap();
        std::fs::remove_file(d.join("gone.txt")).unwrap();
        std::fs::write(d.join("new file.md"), "# hi\n\ntext").unwrap();
        std::fs::create_dir(d.join("target")).unwrap();
        std::fs::write(d.join("target/junk"), "ignored").unwrap();
        std::fs::write(d.join("blob.bin"), [0u8, 159, 146, 150]).unwrap();

        let s = git_status(d).await;
        assert!(s.is_repo);
        assert_eq!(s.branch.as_deref(), Some("work"));
        assert_eq!(s.base.as_deref(), Some("main"), "measured from where the branch left main");
        assert_eq!(s.ahead, 1);
        assert_eq!(s.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["blob.bin", "gone.txt", "keep.txt", "new file.md", "src/lib.rs"], "sorted; the ignored folder is not listed");

        let lib = change(&s, "src/lib.rs");
        assert_eq!((lib.status, lib.added, lib.removed), (ChangeStatus::Modified, 1, 0), "the committed change counts");
        let keep = change(&s, "keep.txt");
        assert_eq!((keep.status, keep.added, keep.removed), (ChangeStatus::Modified, 2, 1));
        assert_eq!(change(&s, "gone.txt").status, ChangeStatus::Deleted);
        assert_eq!(change(&s, "gone.txt").removed, 1);
        let new = change(&s, "new file.md");
        assert_eq!((new.status, new.added), (ChangeStatus::Untracked, 3), "a last line without a newline still counts");
        assert!(change(&s, "blob.bin").binary);
        assert_eq!(s.added, 1 + 2 + 3);
        assert_eq!(s.removed, 1 + 1);
    }

    #[tokio::test]
    async fn on_the_main_line_itself_only_uncommitted_work_is_listed() {
        let tmp = repo().await;
        let d = tmp.path();
        sh(d, &["checkout", "-q", "main"]).await;
        assert!(git_status(d).await.files.is_empty(), "a clean checkout has no changes");
        std::fs::write(d.join("keep.txt"), "one\n").unwrap();
        let s = git_status(d).await;
        assert_eq!(s.base, None, "there is nothing to measure against but the last commit");
        assert_eq!(s.files.len(), 1);
        assert_eq!(s.ahead, 0);
    }

    #[tokio::test]
    async fn a_folder_that_is_not_a_repository_and_one_with_no_commits_yet() {
        let tmp = tempfile::tempdir().unwrap();
        assert_eq!(git_status(tmp.path()).await, GitSummary::default());

        sh(tmp.path(), &["init", "-q", "-b", "main"]).await;
        std::fs::write(tmp.path().join("a.txt"), "x\n").unwrap();
        let s = git_status(tmp.path()).await;
        assert!(s.is_repo);
        assert_eq!(s.files.len(), 1, "before the first commit every file is new");
        assert_eq!(s.files[0].status, ChangeStatus::Untracked);
    }

    #[tokio::test]
    async fn a_diff_gives_both_sides_and_a_patch() {
        let tmp = repo().await;
        let d = tmp.path();
        std::fs::write(d.join("keep.txt"), "one\n2\nthree\n").unwrap();
        let diff = git_diff(d, "keep.txt").await.unwrap();
        assert_eq!(diff.old.as_deref(), Some("one\ntwo\nthree\n"));
        assert_eq!(diff.new.as_deref(), Some("one\n2\nthree\n"));
        assert!(diff.unified.contains("-two") && diff.unified.contains("+2"), "{}", diff.unified);
        assert!(!diff.binary && !diff.truncated);

        // a new file has no old side; a deleted one has no new side.
        std::fs::write(d.join("fresh.txt"), "hello\n").unwrap();
        let fresh = git_diff(d, "fresh.txt").await.unwrap();
        assert_eq!((fresh.old, fresh.new.as_deref()), (None, Some("hello\n")));
        assert!(fresh.unified.contains("+hello"), "{}", fresh.unified);
        std::fs::remove_file(d.join("gone.txt")).unwrap();
        let gone = git_diff(d, "gone.txt").await.unwrap();
        assert_eq!((gone.old.as_deref(), gone.new), (Some("bye\n"), None));

        // binary files are flagged, not dumped.
        std::fs::write(d.join("blob.bin"), [1u8, 0, 2]).unwrap();
        let blob = git_diff(d, "blob.bin").await.unwrap();
        assert!(blob.binary && blob.old.is_none() && blob.new.is_none());

        assert_eq!(git_diff(d, "never-existed.txt").await.unwrap_err().code, ErrorCode::NotFound);
    }

    #[tokio::test]
    async fn nothing_can_reach_outside_the_session_folder() {
        let outer = tempfile::tempdir().unwrap();
        std::fs::write(outer.path().join("secret.txt"), "do not read").unwrap();
        let root = outer.path().join("session");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("ok.txt"), "fine").unwrap();
        std::os::unix::fs::symlink(outer.path().join("secret.txt"), root.join("link-to-secret")).unwrap();
        std::os::unix::fs::symlink(outer.path(), root.join("link-to-parent")).unwrap();

        for bad in ["../secret.txt", "a/../../secret.txt", "/etc/passwd", "link-to-secret", "link-to-parent/secret.txt", "link-to-parent/new.txt"] {
            assert_eq!(read(&root, bad).await.unwrap_err().code, ErrorCode::Invalid, "read {bad}");
            assert_eq!(write(&root, bad, "pwned").await.unwrap_err().code, ErrorCode::Invalid, "write {bad}");
            assert!(list(&root, bad).await.is_err(), "list {bad}");
        }
        assert_eq!(std::fs::read_to_string(outer.path().join("secret.txt")).unwrap(), "do not read");
        assert!(!outer.path().join("new.txt").exists());
        // ordinary paths, including ones with harmless dots, still work.
        assert_eq!(read(&root, "./ok.txt").await.unwrap().text.as_deref(), Some("fine"));
        assert!(resolve(&root, "sub/dir/new.txt").is_ok(), "a path that doesn't exist yet is fine if it stays inside");
    }

    #[tokio::test]
    async fn listing_reading_and_saving_files() {
        let tmp = repo().await;
        let d = tmp.path();
        let top = list(d, "").await.unwrap();
        assert_eq!(top.iter().map(|e| (e.name.as_str(), e.is_dir)).collect::<Vec<_>>(), [("src", true), (".gitignore", false), ("gone.txt", false), ("keep.txt", false)], "folders first, .git hidden");
        assert_eq!(list(d, "src").await.unwrap().len(), 1);
        assert_eq!(list(d, "nope").await.unwrap_err().code, ErrorCode::NotFound);

        let f = read(d, "keep.txt").await.unwrap();
        assert_eq!((f.text.as_deref(), f.size, f.binary, f.truncated), (Some("one\ntwo\nthree\n"), 14, false, false));
        assert_eq!(read(d, "src").await.unwrap_err().code, ErrorCode::Invalid);

        write(d, "keep.txt", "changed\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "changed\n");
        write(d, "src/new.rs", "// new\n").await.unwrap();
        assert_eq!(std::fs::read_to_string(d.join("src/new.rs")).unwrap(), "// new\n");
        assert_eq!(write(d, "no/such/dir/x.txt", "x").await.unwrap_err().code, ErrorCode::NotFound, "saving does not invent folders");
        assert_eq!(write(d, "src", "x").await.unwrap_err().code, ErrorCode::Invalid);
        // no temporary files are left behind.
        assert!(std::fs::read_dir(d).unwrap().all(|e| !e.unwrap().file_name().to_string_lossy().contains(".photon-")));
    }

    #[tokio::test]
    async fn saving_keeps_a_scripts_executable_bit() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("run.sh");
        std::fs::write(&script, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        write(tmp.path(), "run.sh", "#!/bin/sh\necho hi\n").await.unwrap();
        assert_eq!(std::fs::metadata(&script).unwrap().permissions().mode() & 0o777, 0o755);
    }

    #[tokio::test]
    async fn a_huge_file_is_cut_and_says_so() {
        let tmp = tempfile::tempdir().unwrap();
        // multi-byte characters, so a careless cut would land inside one.
        std::fs::write(tmp.path().join("big.txt"), "é".repeat(MAX_FILE)).unwrap();
        let f = read(tmp.path(), "big.txt").await.unwrap();
        assert!(f.truncated);
        assert_eq!(f.size as usize, 2 * MAX_FILE);
        let text = f.text.unwrap();
        assert!(text.len() <= MAX_FILE && text.len() >= MAX_FILE - 4);
        assert!(text.chars().all(|c| c == 'é'), "no broken character at the cut");
    }

    #[test]
    fn pull_request_checks_are_read_in_both_shapes_github_uses() {
        let json = br#"{"number": 42, "title": "Fix login", "url": "https://github.com/o/r/pull/42", "state": "OPEN", "isDraft": true,
            "baseRefName": "main", "headRefName": "fix-login", "statusCheckRollup": [
              {"__typename": "CheckRun", "name": "test", "status": "COMPLETED", "conclusion": "SUCCESS", "detailsUrl": "https://ci/1"},
              {"__typename": "CheckRun", "name": "lint", "status": "COMPLETED", "conclusion": "FAILURE"},
              {"__typename": "CheckRun", "name": "build", "status": "IN_PROGRESS", "conclusion": ""},
              {"__typename": "CheckRun", "name": "docs", "status": "COMPLETED", "conclusion": "SKIPPED"},
              {"__typename": "StatusContext", "context": "deploy/preview", "state": "PENDING", "targetUrl": "https://preview"},
              {"__typename": "StatusContext", "context": "legacy", "state": "ERROR"},
              {"__typename": "CheckRun", "name": "odd", "status": "COMPLETED", "conclusion": "SOMETHING_NEW"}
            ]}"#;
        let pr = parse_pr(json).unwrap();
        assert_eq!((pr.number, pr.title.as_str(), pr.state.as_str(), pr.draft), (42, "Fix login", "open", true));
        assert_eq!((pr.base.as_str(), pr.head.as_str()), ("main", "fix-login"));
        let states: Vec<_> = pr.checks.iter().map(|c| (c.name.as_str(), c.state)).collect();
        assert_eq!(
            states,
            [
                ("test", CheckState::Passing),
                ("lint", CheckState::Failing),
                ("build", CheckState::Pending),
                ("docs", CheckState::Skipped),
                ("deploy/preview", CheckState::Pending),
                ("legacy", CheckState::Failing),
                ("odd", CheckState::Unknown),
            ]
        );
        assert_eq!(pr.checks[0].url.as_deref(), Some("https://ci/1"));
        assert_eq!(pr.checks[4].url.as_deref(), Some("https://preview"));
        // a pull request with no checks, and junk, are both handled.
        assert!(parse_pr(br#"{"number": 1, "title": "t", "url": "u", "state": "MERGED"}"#).unwrap().checks.is_empty());
        assert!(parse_pr(b"not json").is_none());
        assert!(parse_pr(b"{}").is_none());
    }

    #[tokio::test]
    async fn no_pull_request_is_explained_not_an_error() {
        let tmp = repo().await;
        let status = pr_status(tmp.path()).await;
        assert!(status.pr.is_none());
        assert!(status.note.is_some_and(|n| !n.is_empty()), "there is always a reason to show");
    }
}
