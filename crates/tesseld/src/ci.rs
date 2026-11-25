use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub enum Ci {
    Pending,
    Passed,
    Failed(Vec<(String, String)>),
    Merged(String),
    Closed,
}

async fn gh(dir: &Path, args: &[&str], secs: u64) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args).current_dir(dir).stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(secs), cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(Ok(o)) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        Ok(Err(e)) => Err(format!("couldn't run gh: {e}")),
        Err(_) => Err("gh took too long".into()),
    }
}

pub fn read(v: &serde_json::Value) -> Ci {
    match v["state"].as_str() {
        Some("MERGED") => return Ci::Merged(v["mergeCommit"]["oid"].as_str().map(|s| s.chars().take(7).collect()).unwrap_or_default()),
        Some("CLOSED") => return Ci::Closed,
        _ => {}
    }
    let checks = v["statusCheckRollup"].as_array().cloned().unwrap_or_default();
    let mut failed = Vec::new();
    let mut waiting = false;
    for c in &checks {
        let name = c["name"].as_str().or(c["context"].as_str()).unwrap_or("a check").to_owned();
        let link = c["detailsUrl"].as_str().or(c["targetUrl"].as_str()).unwrap_or_default().to_owned();
        let outcome = c["conclusion"].as_str().filter(|s| !s.is_empty()).or(c["state"].as_str()).unwrap_or("").to_ascii_uppercase();
        let done = c["status"].as_str().is_none_or(|s| s.eq_ignore_ascii_case("COMPLETED")) && !outcome.is_empty();
        match outcome.as_str() {
            "FAILURE" | "ERROR" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE" if done => failed.push((name, link)),
            "SUCCESS" | "NEUTRAL" | "SKIPPED" if done => {}
            _ => waiting = true,
        }
    }
    if !failed.is_empty() {
        Ci::Failed(failed)
    } else if waiting {
        Ci::Pending
    } else {
        Ci::Passed
    }
}

pub async fn poll(repo: &Path, url: &str) -> Result<Ci, String> {
    let out = gh(repo, &["pr", "view", url, "--json", "state,mergeCommit,statusCheckRollup"], 60).await?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| format!("gh's answer: {e}"))?;
    Ok(read(&v))
}

pub async fn open_pr(ws: &Path) -> Option<String> {
    crate::ship::web_url(&ws.to_string_lossy())?;
    let out = gh(ws, &["pr", "view", "--json", "url,state"], 60).await.ok()?;
    let v: serde_json::Value = serde_json::from_str(&out).ok()?;
    (v["state"].as_str() == Some("OPEN")).then(|| v["url"].as_str().map(str::to_owned)).flatten()
}

pub fn run_id(link: &str) -> Option<&str> {
    let rest = link.split("/actions/runs/").nth(1)?;
    let id = rest.split(['/', '?', '#']).next()?;
    id.chars().all(|c| c.is_ascii_digit()).then_some(id).filter(|s| !s.is_empty())
}

pub async fn why(repo: &Path, failed: &[(String, String)]) -> String {
    let mut out = String::new();
    let mut seen = Vec::new();
    for (name, link) in failed {
        out.push_str(&format!("✗ {name}\n"));
        let Some(id) = run_id(link) else { continue };
        if seen.contains(&id) {
            continue;
        }
        seen.push(id);
        if let Ok(log) = gh(repo, &["run", "view", id, "--log-failed"], 90).await {
            out.push_str(&crate::proof::trimmed(&log));
            out.push('\n');
        }
    }
    out.trim_end().to_owned()
}

pub async fn merge(repo: &Path, url: &str, closes: Option<u64>) -> Result<Ci, String> {
    let body = closes.map(|n| format!("Closes #{n}")).unwrap_or_default();
    gh(repo, &["pr", "merge", url, "--squash", "--body", &body], 120).await?;
    for _ in 0..5 {
        if let Ok(Ci::Merged(sha)) = poll(repo, url).await {
            return Ok(Ci::Merged(sha));
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
    Ok(Ci::Merged(String::new()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_pull_request_s_checks_read_as_waiting_passed_or_failed_with_where_to_look() {
        let pr = |checks: serde_json::Value| json!({"state": "OPEN", "statusCheckRollup": checks});
        assert_eq!(read(&pr(json!([]))), Ci::Passed, "a repository without checks has nothing to wait for");
        assert_eq!(read(&pr(json!([{"name": "test", "status": "IN_PROGRESS", "conclusion": ""}]))), Ci::Pending);
        assert_eq!(read(&pr(json!([{"name": "test", "status": "COMPLETED", "conclusion": "SUCCESS"}, {"context": "vercel", "state": "SUCCESS"}]))), Ci::Passed);
        assert_eq!(
            read(&pr(json!([{"name": "lint", "status": "COMPLETED", "conclusion": "SUCCESS"}, {"name": "test", "status": "COMPLETED", "conclusion": "FAILURE", "detailsUrl": "https://github.com/o/r/actions/runs/123/job/9"}, {"name": "e2e", "status": "QUEUED", "conclusion": ""}]))),
            Ci::Failed(vec![("test".into(), "https://github.com/o/r/actions/runs/123/job/9".into())]),
            "one failure is enough to act on, without waiting for the rest"
        );
        assert_eq!(read(&json!({"state": "MERGED", "mergeCommit": {"oid": "abcdef123456"}})), Ci::Merged("abcdef1".into()));
        assert_eq!(read(&json!({"state": "CLOSED"})), Ci::Closed);
        assert_eq!(run_id("https://github.com/o/r/actions/runs/123/job/9"), Some("123"));
        assert_eq!(run_id("https://vercel.com/x"), None);
    }
}
