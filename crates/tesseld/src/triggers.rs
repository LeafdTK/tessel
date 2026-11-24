use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cron {
    minute: Vec<bool>,
    hour: Vec<bool>,
    dom: Vec<bool>,
    month: Vec<bool>,
    dow: Vec<bool>,
    dom_any: bool,
    dow_any: bool,
}

fn field(spec: &str, lo: u32, hi: u32, names: &[&str]) -> Result<(Vec<bool>, bool), String> {
    let mut on = vec![false; hi as usize + 1];
    let any = spec == "*";
    let value = |s: &str| -> Result<u32, String> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return Ok(i as u32 + lo);
        }
        s.parse::<u32>().map_err(|_| format!("\"{s}\" isn't a number"))
    };
    for part in spec.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, s.parse::<u32>().map_err(|_| format!("\"{s}\" isn't a step"))?.max(1)),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (value(a)?, value(b)?)
        } else {
            let v = value(range)?;
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("{range} is outside {lo}-{hi}"));
        }
        let mut v = a;
        while v <= b {
            on[v as usize] = true;
            v += step;
        }
    }
    Ok((on, any))
}

impl Cron {
    pub fn parse(spec: &str) -> Result<Self, String> {
        let parts: Vec<&str> = spec.split_whitespace().collect();
        let [m, h, dom, mon, dow] = parts.as_slice() else { return Err("a schedule has five parts: minute hour day month weekday".into()) };
        let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
        let days = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
        let (mut dow_on, dow_any) = field(dow, 0, 7, &days)?;
        if dow_on[7] {
            dow_on[0] = true;
        }
        let (dom_on, dom_any) = field(dom, 1, 31, &[])?;
        Ok(Cron { minute: field(m, 0, 59, &[])?.0, hour: field(h, 0, 23, &[])?.0, dom: dom_on, month: field(mon, 1, 12, &months)?.0, dow: dow_on, dom_any, dow_any })
    }

    pub fn matches(&self, t: &Local) -> bool {
        let day = match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => self.dom[t.day as usize],
            (true, false) => self.dow[t.weekday as usize],
            (false, false) => self.dom[t.day as usize] || self.dow[t.weekday as usize],
        };
        self.minute[t.minute as usize] && self.hour[t.hour as usize] && self.month[t.month as usize] && day
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub weekday: u32,
}

impl Local {
    pub fn at(unix_secs: i64) -> Self {
        let mut tm: libc::tm = unsafe { std::mem::zeroed() };
        let t = unix_secs as libc::time_t;
        unsafe { libc::localtime_r(&t, &mut tm) };
        Local { year: tm.tm_year + 1900, month: tm.tm_mon as u32 + 1, day: tm.tm_mday as u32, hour: tm.tm_hour as u32, minute: tm.tm_min as u32, weekday: tm.tm_wday as u32 }
    }

    pub fn stamp(&self) -> String {
        format!("{:04}{:02}{:02}{:02}{:02}", self.year, self.month, self.day, self.hour, self.minute)
    }
}

const SKIP: [&str; 6] = [".git", "node_modules", "target", ".next", "dist", ".venv"];
const MOST: usize = 20_000;

fn walk_root(folder: &Path, glob: &str) -> PathBuf {
    let fixed: Vec<&str> = glob.split('/').take_while(|p| !p.contains(['*', '?', '[', '{'])).collect();
    let fixed = if fixed.len() == glob.split('/').count() { &fixed[..fixed.len().saturating_sub(1)] } else { &fixed[..] };
    folder.join(fixed.join("/"))
}

pub fn snapshot(folder: &Path, globs: &[String]) -> BTreeMap<String, u128> {
    let mut b = globset::GlobSetBuilder::new();
    for g in globs {
        if let Ok(g) = globset::Glob::new(g) {
            b.add(g);
        }
    }
    let Ok(set) = b.build() else { return BTreeMap::new() };
    let mut out = BTreeMap::new();
    let mut seen = 0;
    let mut stack: Vec<PathBuf> = globs.iter().map(|g| walk_root(folder, g)).collect();
    stack.sort();
    stack.dedup();
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            seen += 1;
            if seen > MOST {
                return out;
            }
            let p = e.path();
            let Ok(meta) = e.metadata() else { continue };
            if meta.is_dir() {
                if !SKIP.iter().any(|s| e.file_name() == *s) {
                    stack.push(p);
                }
                continue;
            }
            let Ok(rel) = p.strip_prefix(folder) else { continue };
            let rel = rel.to_string_lossy().into_owned();
            if set.is_match(&rel) {
                let at = meta.modified().ok().and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
                out.insert(rel, at ^ u128::from(meta.len()));
            }
        }
    }
    out
}

pub fn changed(before: &BTreeMap<String, u128>, now: &BTreeMap<String, u128>) -> Vec<String> {
    let mut out: Vec<String> = now.iter().filter(|(k, v)| before.get(*k) != Some(v)).map(|(k, _)| k.clone()).collect();
    out.extend(before.keys().filter(|k| !now.contains_key(*k)).cloned());
    out.sort();
    out
}

#[derive(Default)]
pub struct Watch {
    pub base: Option<BTreeMap<String, u128>>,
    pub pending: Option<BTreeMap<String, u128>>,
}

impl Watch {
    pub fn step(&mut self, now: BTreeMap<String, u128>) -> Option<Vec<String>> {
        let Some(base) = &self.base else {
            self.base = Some(now);
            return None;
        };
        if changed(base, &now).is_empty() {
            self.pending = None;
            return None;
        }
        match &self.pending {
            Some(p) if *p == now => {
                let files = changed(base, &now);
                self.base = Some(now);
                self.pending = None;
                Some(files)
            }
            _ => {
                self.pending = Some(now);
                None
            }
        }
    }
}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct Fired {
    pub keys: HashMap<String, u64>,
}

impl Fired {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn first(&mut self, key: &str, now_ms: u64) -> bool {
        if self.keys.contains_key(key) {
            return false;
        }
        self.keys.insert(key.to_owned(), now_ms);
        if self.keys.len() > 5000 {
            let mut all: Vec<(String, u64)> = self.keys.drain().collect();
            all.sort_by_key(|(_, t)| std::cmp::Reverse(*t));
            all.truncate(2500);
            self.keys = all.into_iter().collect();
        }
        true
    }

    pub fn save(&self, path: &Path) {
        if let Ok(t) = serde_json::to_string(self) {
            let _ = std::fs::write(path, t);
        }
    }
}

pub async fn labeled(repo: &str, label: &str) -> Result<Vec<serde_json::Value>, String> {
    let out = tokio::process::Command::new("gh")
        .args(["api", &format!("repos/{repo}/issues?labels={label}&state=open&per_page=30")])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| format!("gh: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| e.to_string())?;
    Ok(v.as_array().cloned().unwrap_or_default())
}

pub fn issue_event(repo: &str, issue: &serde_json::Value) -> serde_json::Value {
    let s = |k: &str| issue.get(k).and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
    serde_json::json!({
        "title": s("title"),
        "body": s("body").chars().take(8000).collect::<String>(),
        "url": s("html_url"),
        "number": issue.get("number").cloned().unwrap_or_default(),
        "repo": repo,
        "kind": if issue.get("pull_request").is_some() { "pull request" } else { "issue" },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(y: i32, mo: u32, d: u32, h: u32, mi: u32, wd: u32) -> Local {
        Local { year: y, month: mo, day: d, hour: h, minute: mi, weekday: wd }
    }

    #[test]
    fn schedules_read_like_cron() {
        let weekdays = Cron::parse("0 9 * * 1-5").unwrap();
        assert!(weekdays.matches(&at(2026, 11, 23, 9, 0, 1)));
        assert!(!weekdays.matches(&at(2026, 11, 22, 9, 0, 0)), "not on sunday");
        assert!(!weekdays.matches(&at(2026, 11, 23, 9, 1, 1)));
        let every15 = Cron::parse("*/15 * * * *").unwrap();
        assert!(every15.matches(&at(2026, 1, 1, 3, 45, 4)) && !every15.matches(&at(2026, 1, 1, 3, 46, 4)));
        let named = Cron::parse("30 8 1 jan,jul sun").unwrap();
        assert!(named.matches(&at(2026, 7, 1, 8, 30, 3)), "day of month or weekday, as cron does");
        assert!(named.matches(&at(2026, 1, 4, 8, 30, 0)));
        assert!(!named.matches(&at(2026, 2, 1, 8, 30, 0)));
        assert!(Cron::parse("0 7 * * 7").unwrap().matches(&at(2026, 11, 22, 7, 0, 0)), "7 is sunday too");
        assert!(Cron::parse("61 * * * *").is_err());
        assert!(Cron::parse("0 9 * *").unwrap_err().contains("five parts"));
        assert!(Cron::parse("0 9 * * funday").is_err());
    }

    #[test]
    fn local_time_has_a_stamp_per_minute() {
        let t = Local::at(1_795_000_000);
        assert_eq!(t.stamp().len(), 12);
        assert!(t.weekday < 7 && t.month >= 1 && t.month <= 12);
    }

    #[test]
    fn file_changes_fire_once_they_settle() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        std::fs::create_dir_all(docs.join("deep")).unwrap();
        std::fs::create_dir_all(dir.path().join("node_modules/x")).unwrap();
        std::fs::write(docs.join("a.md"), "a").unwrap();
        std::fs::write(dir.path().join("readme.txt"), "x").unwrap();
        let globs = vec!["docs/**/*.md".to_owned()];
        let mut w = Watch::default();
        assert_eq!(w.step(snapshot(dir.path(), &globs)), None, "the first look is where it starts from");
        std::fs::write(docs.join("deep/b.md"), "b").unwrap();
        std::fs::write(dir.path().join("readme.txt"), "y").unwrap();
        assert_eq!(w.step(snapshot(dir.path(), &globs)), None, "still settling");
        assert_eq!(w.step(snapshot(dir.path(), &globs)), Some(vec!["docs/deep/b.md".to_owned()]), "only what the globs cover");
        assert_eq!(w.step(snapshot(dir.path(), &globs)), None, "and once");
        std::fs::remove_file(docs.join("a.md")).unwrap();
        w.step(snapshot(dir.path(), &globs));
        assert_eq!(w.step(snapshot(dir.path(), &globs)), Some(vec!["docs/a.md".to_owned()]), "a removal counts");
        assert_eq!(walk_root(dir.path(), "docs/**/*.md"), dir.path().join("docs"));
        assert_eq!(walk_root(dir.path(), "*.md"), dir.path().to_path_buf());
    }

    #[test]
    fn an_event_fires_once_whatever_happens_after() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fired.json");
        let mut f = Fired::default();
        assert!(f.first("cron:x:202611230900", 1));
        assert!(!f.first("cron:x:202611230900", 2));
        f.save(&path);
        assert!(!Fired::load(&path).first("cron:x:202611230900", 3), "a restart doesn't fire it again");
        let issue = serde_json::json!({"title": "Fix it", "body": "pls", "html_url": "https://github.com/o/r/issues/4", "number": 4, "pull_request": {}});
        let ev = issue_event("o/r", &issue);
        assert_eq!((ev["title"].as_str(), ev["kind"].as_str(), ev["number"].as_i64()), (Some("Fix it"), Some("pull request"), Some(4)));
    }
}
