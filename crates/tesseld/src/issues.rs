use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Issue {
    pub number: u64,
    pub url: String,
    pub title: String,
    pub body: String,
}

pub fn parse_ref(s: &str) -> Option<(Option<String>, u64)> {
    let s = s.trim();
    if let Some(rest) = s.split_once("github.com/").map(|(_, r)| r) {
        let parts: Vec<&str> = rest.split(['/', '?', '#']).collect();
        if parts.len() >= 4 && (parts[2] == "issues" || parts[2] == "pull") {
            return Some((Some(format!("{}/{}", parts[0], parts[1])), parts[3].parse().ok()?));
        }
        return None;
    }
    let n = s.strip_prefix('#').unwrap_or(s);
    (!n.is_empty() && n.chars().all(|c| c.is_ascii_digit())).then(|| n.parse().ok()).flatten().map(|n| (None, n))
}

pub async fn slug(root: &Path) -> Option<String> {
    let home = crate::ship::home(root).await;
    let url = crate::ship::git(root, &["remote", "get-url", &home.remote]).await.ok()?;
    crate::ship::github_repo(&url)
}

pub async fn closes(root: &Path, issue_url: &str) -> Option<u64> {
    let (repo, n) = parse_ref(issue_url)?;
    let here = slug(root).await?;
    (repo.is_none_or(|r| r.eq_ignore_ascii_case(&here))).then_some(n)
}

async fn gh(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = tokio::process::Command::new("gh");
    cmd.args(args).current_dir(dir).stdin(std::process::Stdio::null()).kill_on_drop(true);
    match tokio::time::timeout(std::time::Duration::from_secs(60), cmd.output()).await {
        Ok(Ok(o)) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(Ok(o)) => Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()),
        Ok(Err(e)) => Err(format!("couldn't run gh: {e}")),
        Err(_) => Err("gh took too long".into()),
    }
}

fn read(v: &serde_json::Value) -> Option<Issue> {
    Some(Issue {
        number: v["number"].as_u64()?,
        url: v["url"].as_str()?.to_owned(),
        title: v["title"].as_str()?.trim().to_owned(),
        body: v["body"].as_str().unwrap_or_default().trim().chars().take(8000).collect(),
    })
}

pub async fn fetch(root: &Path, which: &str) -> Result<Issue, String> {
    let (repo, n) = parse_ref(which).ok_or_else(|| format!("\"{which}\" isn't an issue: give its number, like #42, or its link"))?;
    let repo = match repo {
        Some(r) => r,
        None => slug(root).await.ok_or("this repository isn't on GitHub")?,
    };
    let out = gh(root, &["issue", "view", &n.to_string(), "--repo", &repo, "--json", "number,url,title,body,state"]).await?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    if v["state"].as_str() == Some("CLOSED") {
        return Err(format!("#{n} is closed"));
    }
    read(&v).ok_or_else(|| "GitHub's answer didn't make sense".into())
}

pub async fn labeled(root: &Path, label: &str) -> Result<Vec<Issue>, String> {
    let repo = slug(root).await.ok_or("this repository isn't on GitHub")?;
    let out = gh(root, &["issue", "list", "--repo", &repo, "--label", label, "--state", "open", "--limit", "30", "--json", "number,url,title,body"]).await?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(v.as_array().map(|a| a.iter().filter_map(read).collect()).unwrap_or_default())
}

pub async fn open(root: &Path) -> Result<Vec<tessel_proto::task::IssueInfo>, String> {
    let repo = slug(root).await.ok_or("this project isn't on GitHub")?;
    let out = gh(root, &["issue", "list", "--repo", &repo, "--state", "open", "--limit", "40", "--json", "number,url,title,labels"]).await.map_err(|e| if e.contains("couldn't run gh") { "GitHub's gh isn't installed where tessel runs".into() } else { e })?;
    let v: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    Ok(v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    Some(tessel_proto::task::IssueInfo {
                        number: i["number"].as_u64()?,
                        title: i["title"].as_str()?.to_owned(),
                        url: i["url"].as_str()?.to_owned(),
                        labels: i["labels"].as_array().map(|l| l.iter().filter_map(|x| x["name"].as_str().map(str::to_owned)).collect()).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn spec(issue: &Issue) -> String {
    if issue.body.is_empty() { format!("From {}", issue.url) } else { format!("{}\n\nFrom {}", issue.body, issue.url) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_issue_is_named_by_its_number_or_its_link() {
        assert_eq!(parse_ref("#42"), Some((None, 42)));
        assert_eq!(parse_ref(" 7 "), Some((None, 7)));
        assert_eq!(parse_ref("https://github.com/LeafdTK/tessel/issues/12"), Some((Some("LeafdTK/tessel".into()), 12)));
        assert_eq!(parse_ref("https://github.com/o/r/issues/3#issuecomment-1"), Some((Some("o/r".into()), 3)));
        assert_eq!(parse_ref("Add dark mode"), None);
        assert_eq!(parse_ref("#"), None);
        assert_eq!(parse_ref("https://github.com/o/r"), None);
        let i = Issue { number: 3, url: "https://github.com/o/r/issues/3".into(), title: "t".into(), body: String::new() };
        assert_eq!(spec(&i), "From https://github.com/o/r/issues/3");
    }
}
