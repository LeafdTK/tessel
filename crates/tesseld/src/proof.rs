use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tessel_agent::Backend;
use tessel_proto::proof::{Check, Finding, Knowledge, Proof, Reviewer, Rule, Rulebook, Scenario, Severity, Tried, Verdict, Verified, Within};

use crate::ship::git;

const DIFF_CAP: usize = 160 * 1024;
const OUTPUT_CAP: usize = 6 * 1024;
const PER_RULE: usize = 5;

#[derive(Clone, Debug, Default)]
pub struct Change {
    pub base: String,
    pub head: String,
    pub diff: String,
    pub added: Vec<(String, u32, String)>,
    pub changed: usize,
    pub paths: Vec<String>,
    pub subjects: Vec<String>,
    pub history: Vec<String>,
}

impl Change {
    pub fn fingerprint(&self) -> String {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.head.hash(&mut h);
        self.diff.hash(&mut h);
        format!("{:016x}", h.finish())
    }
}

#[derive(Clone, Debug, Default)]
pub struct Ask {
    pub goal: String,
    pub said: Vec<String>,
}

pub async fn base_of(ws: &Path, main: &str) -> Option<String> {
    let mut candidates = Vec::new();
    if let Ok(branch) = git(ws, &["rev-parse", "--abbrev-ref", "HEAD"]).await {
        candidates.push(crate::worktree::base_ref(&branch));
    }
    candidates.push(crate::ship::home(ws).await.upstream);
    candidates.push(main.to_owned());
    let mut best: Option<(u64, String)> = None;
    for c in candidates {
        let Ok(mb) = git(ws, &["merge-base", "HEAD", &c]).await else { continue };
        let Ok(n) = git(ws, &["rev-list", "--count", &format!("{mb}..HEAD")]).await else { continue };
        let n: u64 = n.trim().parse().unwrap_or(u64::MAX);
        if best.as_ref().is_none_or(|(b, _)| n < *b) {
            best = Some((n, mb));
        }
    }
    best.map(|(_, mb)| mb)
}

pub async fn change(ws: &Path) -> Result<Change, String> {
    let main = crate::ship::main_branch(ws).await;
    let head = git(ws, &["rev-parse", "HEAD"]).await?;
    let base = base_of(ws, &main).await.unwrap_or_else(|| head.clone());
    let local = format!(":(exclude){}", crate::setup::LOCAL);
    let mut diff = git(ws, &["diff", "--no-color", "--no-ext-diff", "-U3", &base, "--", ".", &local]).await?;
    let mut paths: Vec<String> = git(ws, &["diff", "--name-only", &base, "--", ".", &local]).await?.lines().map(str::to_owned).collect();
    let untracked = git(ws, &["ls-files", "--others", "--exclude-standard"]).await.unwrap_or_default();
    for f in untracked.lines().filter(|f| !f.is_empty() && *f != crate::setup::LOCAL) {
        let Ok(text) = std::fs::read_to_string(ws.join(f)) else { continue };
        if text.len() > 256 * 1024 {
            continue;
        }
        diff.push_str(&format!("\ndiff --git a/{f} b/{f}\nnew file\n--- /dev/null\n+++ b/{f}\n@@ -0,0 +1,{} @@\n", text.lines().count()));
        for l in text.lines() {
            diff.push('+');
            diff.push_str(l);
            diff.push('\n');
        }
        paths.push(f.to_owned());
    }
    let subjects = if base == head { Vec::new() } else { git(ws, &["log", "--format=%s", &format!("{base}..HEAD")]).await.unwrap_or_default().lines().map(str::to_owned).collect() };
    let history = git(ws, &["log", "--format=%s", "-n", "60", &main]).await.unwrap_or_default().lines().map(str::to_owned).collect();
    let added = added_lines(&diff);
    let changed = changed_lines(&diff);
    Ok(Change { base, head, diff, added, changed, paths, subjects, history })
}

pub fn changed_lines(diff: &str) -> usize {
    diff.lines().filter(|l| (l.starts_with('+') && !l.starts_with("+++")) || (l.starts_with('-') && !l.starts_with("---"))).count()
}

pub fn added_lines(diff: &str) -> Vec<(String, u32, String)> {
    let mut out = Vec::new();
    let mut file = String::new();
    let mut line = 0u32;
    for l in diff.lines() {
        if let Some(f) = l.strip_prefix("+++ ") {
            file = f.strip_prefix("b/").unwrap_or(f).to_owned();
        } else if l.starts_with("@@") {
            line = l.split('+').nth(1).and_then(|r| r.split([',', ' ']).next()).and_then(|n| n.parse().ok()).unwrap_or(1);
        } else if l.starts_with("diff --git") || l.starts_with("--- ") {
            continue;
        } else if let Some(t) = l.strip_prefix('+') {
            out.push((file.clone(), line, t.to_owned()));
            line += 1;
        } else if !l.starts_with('-') && !l.starts_with('\\') {
            line += 1;
        }
    }
    out
}

pub fn matched(rules: &[Rule], c: &Change) -> Vec<Finding> {
    let mut out = Vec::new();
    for r in rules {
        let Check::Match { pattern, within, must } = &r.check else { continue };
        let Ok(re) = regex::Regex::new(pattern) else { continue };
        let finding = |file: Option<String>, line: Option<u32>, why: String| Finding {
            lens: "rules".into(),
            rule: Some(r.id.clone()),
            file,
            line,
            severity: r.severity.clone(),
            title: r.title.clone(),
            why,
            confirmed: Some(true),
        };
        let mut found = Vec::new();
        match within {
            Within::Added => {
                for (file, line, text) in &c.added {
                    if !*must && crate::rules::applies(&r.paths, std::slice::from_ref(file)) && re.is_match(text) {
                        found.push(finding(Some(file.clone()), Some(*line), text.trim().chars().take(160).collect()));
                    }
                }
            }
            Within::Subjects => {
                for s in &c.subjects {
                    if re.is_match(s) != *must {
                        found.push(finding(None, None, format!("commit \"{s}\"")));
                    }
                }
            }
            Within::Paths => {
                for p in &c.paths {
                    if !*must && crate::rules::applies(&r.paths, std::slice::from_ref(p)) && re.is_match(p) {
                        found.push(finding(Some(p.clone()), None, String::new()));
                    }
                }
            }
        }
        out.extend(found.into_iter().take(PER_RULE));
    }
    out
}

pub fn trimmed(output: &str) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let loud = |l: &str| {
        let l = l.to_ascii_lowercase();
        l.contains("error") || l.contains("failed") || l.contains("panicked") || l.contains("fail:") || l.contains("assert")
    };
    let mut keep: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| loud(l)).map(|(i, _)| i).take(40).collect();
    keep.extend(lines.len().saturating_sub(20)..lines.len());
    keep.sort_unstable();
    keep.dedup();
    let mut out = String::new();
    let mut last = None;
    for i in keep {
        if last.is_some_and(|l| l + 1 != i) {
            out.push_str("…\n");
        }
        out.push_str(lines[i]);
        out.push('\n');
        last = Some(i);
    }
    if out.len() > OUTPUT_CAP {
        let cut = out.len() - OUTPUT_CAP;
        let cut = (cut..out.len()).find(|&i| out.is_char_boundary(i)).unwrap_or(out.len());
        out = format!("…{}", &out[cut..]);
    }
    out.trim_end().to_owned()
}

pub async fn run_check(ws: &Path, command: &str, rule: Option<String>) -> Verified {
    run_with(ws, command, rule, &[]).await
}

pub async fn run_with(ws: &Path, command: &str, rule: Option<String>, env: &[(&str, &str)]) -> Verified {
    let start = Instant::now();
    let shared = crate::worktree::shared_build(ws, &crate::board::repo_root(ws));
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", command]).current_dir(ws).env("PYTHONDONTWRITEBYTECODE", "1").envs(shared).envs(env.iter().copied()).stdin(std::process::Stdio::null()).kill_on_drop(true);
    let (ok, output) = match tokio::time::timeout(std::time::Duration::from_secs(20 * 60), cmd.output()).await {
        Ok(Ok(o)) => {
            let mut all = String::from_utf8_lossy(&o.stdout).into_owned();
            all.push_str(&String::from_utf8_lossy(&o.stderr));
            (o.status.success(), trimmed(&all))
        }
        Ok(Err(e)) => (false, format!("couldn't run it: {e}")),
        Err(_) => (false, "it ran for 20 minutes and was stopped".into()),
    };
    Verified { command: command.to_owned(), ok, output, ms: start.elapsed().as_millis() as u64, rule }
}

fn lens_brief(lens: &str) -> &'static str {
    match lens {
        "correctness" => "Does the change do what was asked, and does it actually work? Look for logic errors, wrong conditions, off-by-one, broken error handling, races, leaks and misused APIs. Open the surrounding code to confirm before you report.",
        "edge-cases" => "Find inputs and states that break it: empty, huge, malformed, unicode, concurrent, failing dependencies, retries, timeouts. Report missing tests only where a real risk is untested.",
        "security" => "Look for injection, path traversal, missing authorization, secrets, unsafe deserialization, SSRF, cross-tenant access, and untrusted input (including text an agent will read) reaching something powerful.",
        "rules" => "Check the change against each house rule below. Every finding must name the rule's id. Report only real violations by the changed code.",
        "scope" => "Compare the change with what was asked. Report unrelated edits, refactors nobody asked for, weakened or disabled tests, new dependencies without a reason, leftover debugging, and unfinished work (TODOs, stubs).",
        _ => "Review the change for real problems.",
    }
}

pub fn review_prompt(lenses: &[String], ask: &Ask, change: &Change, rules: &[&Rule], knowledge: &[&Knowledge], guidance: &[String]) -> String {
    let mut p = match lenses {
        [one] => format!("You are reviewing a change for one thing only: {one}.\n{}\n", lens_brief(one)),
        many => {
            let mut p = format!("You are reviewing a change for these things: {}.\n", many.join(", "));
            for l in many {
                p.push_str(&format!("\n- {l}: {}", lens_brief(l)));
            }
            p.push('\n');
            p
        }
    };
    p.push_str("\nYou can read any file in this repository (Read, Grep, Glob). You can't change anything. Read only what you need.\n");
    p.push_str(&format!("\n## What was asked\n{}\n", ask.goal));
    for s in ask.said.iter().take(4) {
        p.push_str(&format!("\n> {}\n", s.chars().take(1500).collect::<String>().replace('\n', "\n> ")));
    }
    if !guidance.is_empty() {
        p.push_str(&format!("\n## The repository's own notes for agents\nRead these if they bear on the change: {}\n", guidance.join(", ")));
    }
    if !knowledge.is_empty() {
        p.push_str("\n## How this system works\n");
        for k in knowledge {
            p.push_str(&format!("\n### {} ({})\n{}\n", k.title, k.path, k.body.chars().take(6000).collect::<String>()));
        }
    }
    let judged: Vec<&&Rule> = rules.iter().filter(|r| r.check == Check::Judge).collect();
    if !judged.is_empty() {
        p.push_str("\n## House rules\n");
        for r in judged {
            let sev = match r.severity {
                Severity::Block => "block",
                Severity::Warn => "warn",
                Severity::Note => "note",
            };
            p.push_str(&format!("\n### `{}` ({sev}): {}\n{}\n", r.id, r.title, r.body.chars().take(4000).collect::<String>()));
        }
    }
    let diff = if change.diff.len() > DIFF_CAP {
        let cut = (0..=DIFF_CAP).rev().find(|&i| change.diff.is_char_boundary(i)).unwrap_or(0);
        format!("{}\n… (the rest is cut; read the files for it)", &change.diff[..cut])
    } else {
        change.diff.clone()
    };
    p.push_str(&format!("\n## The change ({} files)\n```diff\n{diff}\n```\n", change.paths.len()));
    p.push_str(
        "\n## Answer\nEnd with a JSON array in a ```json block, and nothing after it. One object per problem:\n\
{\"lens\": \"which of the things above\", \"file\": \"path as in the diff\", \"line\": 42, \"severity\": \"block\" | \"warn\" | \"note\", \"rule\": \"rule id or null\", \"title\": \"under 80 characters\", \"why\": \"the evidence: what goes wrong, and when\"}\n\
`block`: it's wrong or unsafe and must not land. `warn`: it should change. `note`: worth knowing.\n\
Every problem cites a file and a line in the new code. No style preferences unless a house rule asks for them. If you find nothing real, answer `[]`.\n",
    );
    p
}

pub fn recheck_prompt(change: &Change, findings: &[Finding]) -> String {
    let mut p = String::from(
        "Other reviewers reported the problems below in a change. Check each one against the code (Read, Grep, Glob; you can't change anything). \
For each, decide whether the claim is true: does the code really behave the way they say? Refute it only when the code shows the claim is wrong (it misreads the code, the case can't happen, or something else already handles it). Don't refute something because it seems small: lower its severity instead.\n\n## Reported\n",
    );
    for (n, f) in findings.iter().enumerate() {
        p.push_str(&format!(
            "\n{n}. [{}] {}:{} {}\n   {}\n",
            f.lens,
            f.file.as_deref().unwrap_or("?"),
            f.line.map(|l| l.to_string()).unwrap_or_default(),
            f.title,
            f.why
        ));
    }
    let diff: String = change.diff.chars().take(DIFF_CAP / 2).collect();
    p.push_str(&format!("\n## The change\n```diff\n{diff}\n```\n"));
    p.push_str("\n## Answer\nEnd with a JSON array in a ```json block: one {\"n\": 0, \"real\": true, \"severity\": \"block\" | \"warn\" | \"note\", \"note\": \"why\"} per reported problem.\n");
    p
}

pub fn json_tail(text: &str) -> Option<serde_json::Value> {
    if let Some(start) = text.rfind("```json") {
        let rest = &text[start + 7..];
        let end = rest.find("```").unwrap_or(rest.len());
        if let Ok(v) = serde_json::from_str(rest[..end].trim()) {
            return Some(v);
        }
    }
    let start = text.rfind("\n[").map(|i| i + 1).or_else(|| text.trim_start().starts_with('[').then(|| text.len() - text.trim_start().len()))?;
    serde_json::from_str(text[start..].trim()).ok()
}

pub fn parse_findings(lenses: &[String], text: &str, changed: &[String], rule_ids: &[String]) -> Result<Vec<Finding>, String> {
    let first = lenses.first().cloned().unwrap_or_default();
    let Some(serde_json::Value::Array(items)) = json_tail(text) else { return Err("the reviewer didn't answer with a list".into()) };
    let mut out = Vec::new();
    for v in items {
        let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
        let Some(title) = s("title") else { continue };
        let Some(file) = s("file").map(|f| f.trim_start_matches("./").trim_start_matches("b/").to_owned()) else { continue };
        if file.contains('*') || (!changed.contains(&file) && !Path::new(&file).is_relative()) {
            continue;
        }
        let severity = match s("severity").as_deref() {
            Some("block") => Severity::Block,
            Some("warn") => Severity::Warn,
            _ => Severity::Note,
        };
        let rule = s("rule").filter(|r| r != "null" && rule_ids.contains(r));
        let line = v.get("line").and_then(serde_json::Value::as_u64).map(|l| l as u32);
        let lens = s("lens").filter(|l| lenses.contains(l)).unwrap_or_else(|| first.clone());
        out.push(Finding { lens, rule, file: Some(file), line, severity, title: title.chars().take(120).collect(), why: s("why").unwrap_or_default(), confirmed: None });
    }
    Ok(out)
}

pub fn merged(found: Vec<Finding>) -> Vec<Finding> {
    let rank = |s: &Severity| match s {
        Severity::Block => 0,
        Severity::Warn => 1,
        Severity::Note => 2,
    };
    let mut out: Vec<Finding> = Vec::new();
    for f in found {
        let same = out.iter_mut().find(|o| {
            o.file == f.file
                && o.rule == f.rule
                && match (o.line, f.line) {
                    (Some(a), Some(b)) => a.abs_diff(b) <= 1,
                    (a, b) => a == b,
                }
        });
        match same {
            Some(o) => {
                if rank(&f.severity) < rank(&o.severity) {
                    o.severity = f.severity.clone();
                }
                if !o.lens.split(" · ").any(|l| l == f.lens) {
                    o.lens = format!("{} · {}", o.lens, f.lens);
                }
            }
            None => out.push(f),
        }
    }
    out
}

pub fn apply_recheck(findings: &mut [Finding], text: &str) {
    let Some(serde_json::Value::Array(items)) = json_tail(text) else { return };
    for v in items {
        let (Some(n), Some(real)) = (v.get("n").and_then(serde_json::Value::as_u64), v.get("real").and_then(serde_json::Value::as_bool)) else { continue };
        if let Some(f) = findings.get_mut(n as usize) {
            f.confirmed = Some(real);
            let lower = match v.get("severity").and_then(serde_json::Value::as_str) {
                Some("warn") if f.severity == Severity::Block => Some(Severity::Warn),
                Some("note") if f.severity != Severity::Note => Some(Severity::Note),
                _ => None,
            };
            if let Some(s) = lower.filter(|_| real) {
                f.severity = s;
            }
        }
    }
}

pub fn verdict_word(p: &Proof) -> String {
    match p.verdict {
        Verdict::Proving => "proving".into(),
        Verdict::Proven => "proven".into(),
        Verdict::Concerns => "proven with concerns".into(),
        Verdict::Failed => format!("failed, {} blocking", p.blocking()),
        Verdict::Broken => "couldn't be proved".into(),
    }
}

pub fn verdict(p: &Proof) -> Verdict {
    if p.verify.iter().any(|v| !v.ok) || p.tried.iter().any(|t| !t.ok && !t.broken) || p.standing().any(|f| f.severity == Severity::Block) {
        Verdict::Failed
    } else if p.standing().any(|f| f.severity == Severity::Warn) || p.tried.iter().any(|t| t.broken) {
        Verdict::Concerns
    } else if !p.reviewers.is_empty() && p.reviewers.iter().all(|r| r.error.is_some()) && p.verify.is_empty() {
        Verdict::Broken
    } else {
        Verdict::Proven
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Room {
    pub used: f64,
    pub anyway: bool,
}

pub fn room(windows: &[tessel_proto::UsageWindow], now_secs: i64, anyway: bool) -> Room {
    let used = windows.iter().filter(|w| w.resets_at.is_none_or(|r| r > now_secs)).map(|w| w.used).fold(0.0, f64::max);
    Room { used, anyway }
}

pub fn model_for(cfg: &str, calls: &[Vec<String>], room: Room) -> &'static str {
    let picked = match cfg {
        "haiku" => "haiku",
        "opus" => "opus",
        "sonnet" => "sonnet",
        _ if calls.len() == 1 && calls[0].len() <= 3 => "haiku",
        _ => "sonnet",
    };
    if room.used >= 0.8 && !room.anyway { "haiku" } else { picked }
}


#[derive(Clone, Debug, Default)]
pub struct Places {
    pub holdouts: Option<PathBuf>,
    pub shots: Option<PathBuf>,
    pub home: PathBuf,
    pub tessel_home: PathBuf,
    pub no_comments: bool,
}

pub fn big(c: &Change) -> bool {
    c.changed > 60 || c.paths.len() > 4
}

pub fn pick_scenarios(cfg: &crate::rules::Config, all: &[Scenario], c: &Change, risky: bool) -> Vec<Scenario> {
    use crate::rules::Scenarios;
    all.iter()
        .filter(|s| match cfg.scenarios {
            Scenarios::Never => false,
            Scenarios::Always => true,
            Scenarios::Matching if s.paths.is_empty() => s.run.is_some() || big(c) || risky,
            Scenarios::Matching => crate::rules::applies(&s.paths, &c.paths),
        })
        .cloned()
        .collect()
}

pub fn looks(cfg: &crate::rules::Config, c: &Change) -> bool {
    !cfg.look.is_empty() && (cfg.look_paths.is_empty() || crate::rules::applies(&cfg.look_paths, &c.paths))
}

pub fn collect_shots(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png"))).collect();
    out.sort();
    out.truncate(12);
    out
}

fn sev_word(s: &Severity) -> &'static str {
    match s {
        Severity::Block => "block",
        Severity::Warn => "warn",
        Severity::Note => "note",
    }
}

pub fn look_prompt(ask: &Ask, change: &Change, shots: &[PathBuf], rules: &[&Rule]) -> String {
    let mut p = String::from("You are looking at screenshots of a product after a change, to judge how it looks. Open each picture below with Read: it shows you the image. You can't change anything.\n");
    p.push_str(&format!("\n## What was asked\n{}\n", ask.goal));
    p.push_str(&format!("\n## What changed\n{}\n", change.paths.iter().take(30).cloned().collect::<Vec<_>>().join("\n")));
    p.push_str("\n## The screens\n");
    for s in shots {
        p.push_str(&format!("- {}: {}\n", s.file_name().map(|n| n.to_string_lossy()).unwrap_or_default(), s.display()));
    }
    p.push_str(
        "\n## Look for\n\
- what the change was meant to show: is it there, and does it look finished?\n\
- text cut off, overlapping, or running out of its box\n\
- controls cramped against each other or against their container's edge. Everything needs room to breathe; padding that is too tight is a real problem, not a preference\n\
- things that should line up and don't, uneven spacing between things of the same kind\n\
- empty, broken or placeholder states, and text too faint to read\n",
    );
    if !rules.is_empty() {
        p.push_str("\n## House rules for how it looks\n");
        for r in rules {
            p.push_str(&format!("\n### `{}` ({}): {}\n{}\n", r.id, sev_word(&r.severity), r.title, r.body.chars().take(3000).collect::<String>()));
        }
    }
    p.push_str(
        "\n## Answer\nEnd with a JSON array in a ```json block, and nothing after it. One object per problem:\n\
{\"lens\": \"look\", \"file\": \"the screen's file name\", \"severity\": \"block\" | \"warn\" | \"note\", \"rule\": \"rule id or null\", \"title\": \"under 80 characters\", \"why\": \"where on the screen, and what's wrong\"}\n\
`block`: broken or unusable. `warn`: it should change before it lands. `note`: worth knowing. Only what you can see in the pictures. If it all looks right, answer `[]`.\n",
    );
    p
}

pub fn tester_prompt(sc: &Scenario, ask: &Ask, change: &Change) -> String {
    format!(
        "Check that this still works, the way someone using it would, on the code in this folder as it is now.\n\n\
## The scenario: {}\n{}\n\n\
## The change being checked\n{}\nFiles: {}\n\n\
## How\n\
Build and run what you need with Bash, and do what the scenario says. Don't edit, commit or delete the project's files: put anything you make in $TMPDIR. \
Stop servers you start. If something can't be run here (a missing tool, no network to a service), say so: that isn't the change failing.\n\n\
## Answer\nEnd with one ```json block and nothing after it: {{\"passed\": true | false | null, \"said\": \"what you did and saw, with the evidence: commands, output, what was on the page\"}}. \
null means you couldn't tell.\n",
        sc.title,
        sc.body.chars().take(6000).collect::<String>(),
        ask.goal,
        change.paths.iter().take(30).cloned().collect::<Vec<_>>().join(", ")
    )
}

pub fn parse_tried(sc: &Scenario, text: &str, cost_usd: f64, ms: u64) -> Tried {
    let v = json_tail(text);
    let passed = v.as_ref().and_then(|v| v.get("passed")).and_then(serde_json::Value::as_bool);
    let said = v.as_ref().and_then(|v| v.get("said")).and_then(serde_json::Value::as_str).map(str::to_owned).unwrap_or_else(|| text.chars().rev().take(600).collect::<Vec<_>>().into_iter().rev().collect());
    Tried { id: sc.id.clone(), title: sc.title.clone(), ok: passed == Some(true), said: said.chars().take(2000).collect(), ms, cost_usd, holdout: sc.holdout, broken: passed.is_none() }
}

struct Before {
    stash: String,
    untracked: Vec<String>,
    status: String,
}

async fn remember(ws: &Path) -> Before {
    Before {
        stash: git(ws, &["stash", "create"]).await.unwrap_or_default(),
        untracked: git(ws, &["ls-files", "--others", "--exclude-standard"]).await.unwrap_or_default().lines().map(str::to_owned).collect(),
        status: format!("{}\n{}", git(ws, &["status", "--porcelain"]).await.unwrap_or_default(), git(ws, &["diff", "HEAD"]).await.unwrap_or_default()),
    }
}

async fn put_back(ws: &Path, before: &Before) {
    let now = format!("{}\n{}", git(ws, &["status", "--porcelain"]).await.unwrap_or_default(), git(ws, &["diff", "HEAD"]).await.unwrap_or_default());
    if now == before.status {
        return;
    }
    let _ = git(ws, &["reset", "-q", "--hard"]).await;
    if !before.stash.is_empty() {
        let _ = git(ws, &["stash", "apply", "-q", &before.stash]).await;
    }
    for f in git(ws, &["ls-files", "--others", "--exclude-standard"]).await.unwrap_or_default().lines() {
        if !before.untracked.iter().any(|u| u == f) {
            let _ = std::fs::remove_file(ws.join(f));
        }
    }
}

pub async fn try_scenario(backend: &Arc<dyn Backend>, ws: &Path, root: &Path, places: &Places, sc: &Scenario, ask: &Ask, change: &Change, model: &str) -> Tried {
    let start = Instant::now();
    let before = remember(ws).await;
    let sandbox = cfg!(target_os = "macos").then(|| crate::sandbox::tester(ws, root, &places.home, &places.tessel_home));
    let job = tessel_agent::backend::Job {
        prompt: tester_prompt(sc, ask, change),
        cwd: ws.to_path_buf(),
        model: model.to_owned(),
        tools: "Bash,Read,Grep,Glob".into(),
        system: "You test software the way the person using it would, and report exactly what you saw. You never change the project's files.".into(),
        dirs: Vec::new(),
        sandbox,
        act: true,
        minutes: 15,
        env: crate::worktree::shared_build(ws, root),
    };
    let got = backend.job(job).await;
    put_back(ws, &before).await;
    let ms = start.elapsed().as_millis() as u64;
    match got {
        Ok(r) => parse_tried(sc, &r.text, r.cost_usd, ms),
        Err(e) => Tried { id: sc.id.clone(), title: sc.title.clone(), ok: false, said: format!("the tester couldn't finish: {e:#}"), ms, cost_usd: 0.0, holdout: sc.holdout, broken: true },
    }
}

pub fn scripted(sc: &Scenario, v: Verified) -> Tried {
    Tried { id: sc.id.clone(), title: sc.title.clone(), ok: v.ok, said: if v.ok { String::new() } else { format!("{}\n{}", v.command, v.output) }, ms: v.ms, cost_usd: 0.0, holdout: sc.holdout, broken: false }
}

pub async fn prove(backend: Arc<dyn Backend>, ws: PathBuf, root: PathBuf, places: Places, ask: Ask, room: Room, now: impl Fn() -> u64 + Send + Sync + 'static, tell: impl Fn(&Proof) + Send + Sync + 'static) -> Proof {
    let mut p = Proof { started_ms: now(), stage: "Reading the change".into(), ..Default::default() };
    tell(&p);
    let change = match change(&ws).await {
        Ok(c) => c,
        Err(e) => {
            p.verdict = Verdict::Broken;
            p.problem = Some(format!("couldn't read the change: {e}"));
            p.finished_ms = Some(now());
            p.stage.clear();
            tell(&p);
            return p;
        }
    };
    p.head = change.head.clone();
    p.base = change.base.clone();
    p.fingerprint = change.fingerprint();
    p.files = change.paths.len() as u32;
    if change.diff.trim().is_empty() {
        p.verdict = Verdict::Proven;
        p.problem = Some("nothing has changed yet".into());
        p.finished_ms = Some(now());
        p.stage.clear();
        tell(&p);
        return p;
    }
    let book: Rulebook = crate::rules::load_with(&ws, &root, places.holdouts.as_deref());
    let cfg = crate::rules::config(&ws, &root);
    p.risky = !cfg.risk.is_empty() && crate::rules::applies(&cfg.risk, &change.paths);
    let mut rules: Vec<Rule> = book.rules.into_iter().filter(|r| r.check != Check::Unknown).collect();
    if places.no_comments {
        rules.extend(crate::rules::no_comments());
    }
    if cfg.starter
        && let Some(r) = crate::rules::commit_style(&change.history)
        && !rules.iter().any(|x| x.id == r.id)
    {
        rules.push(r);
    }
    let applying: Vec<Rule> = rules.into_iter().filter(|r| matches!(r.check, Check::Match { within: Within::Subjects, .. }) || crate::rules::applies(&r.paths, &change.paths)).collect();
    let knowledge: Vec<Knowledge> = book.knowledge.into_iter().filter(|k| crate::rules::applies(&k.paths, &change.paths)).collect();
    p.rules = applying.iter().map(|r| r.id.clone()).collect();
    p.knowledge = knowledge.iter().map(|k| k.id.clone()).collect();
    p.findings = matched(&applying, &change);
    let finish = |mut p: Proof, why: Option<&str>| {
        if let Some(why) = why {
            p.problem = Some(why.to_owned());
        }
        p.verdict = verdict(&p);
        p.stage.clear();
        p.finished_ms = Some(now());
        tell(&p);
        p
    };
    if p.findings.iter().any(|f| f.severity == Severity::Block) {
        return finish(p, Some("a house rule is broken: fix that first, and the reviewers look after"));
    }

    let picked = pick_scenarios(&cfg, &book.scenarios, &change, p.risky);
    let look_now = looks(&cfg, &change) && places.shots.is_some();
    let mut commands: Vec<(String, Option<String>)> = cfg.verify.iter().map(|c| (c.clone(), None)).collect();
    commands.extend(applying.iter().filter_map(|r| match &r.check {
        Check::Run { command } => Some((command.clone(), Some(r.id.clone()))),
        _ => None,
    }));
    let lab = match &cfg.lab {
        Some(name) if !commands.is_empty() || look_now || picked.iter().any(|s| s.run.is_some()) => {
            p.stage = format!("Sending it to {name}");
            tell(&p);
            match crate::lab::Lab::open(&places.tessel_home, name, &ws, &cfg.lab_setup).await {
                Ok(l) => {
                    p.lab = Some(l.name.clone());
                    Some(l)
                }
                Err(e) => return finish(p, Some(&format!("the lab {name} couldn't take it: {e}"))),
            }
        }
        _ => None,
    };
    for (command, rule) in commands {
        p.stage = format!("Running {command}");
        tell(&p);
        let mut v = match &lab {
            Some(l) => l.run(&command, &[]).await,
            None => run_check(&ws, &command, None).await,
        };
        v.rule = rule;
        p.verify.push(v);
        tell(&p);
    }
    for sc in picked.iter().filter(|s| s.run.is_some()) {
        let command = sc.run.clone().unwrap_or_default();
        p.stage = format!("Trying {}", sc.title);
        tell(&p);
        let v = match &lab {
            Some(l) => l.run(&command, &[]).await,
            None => run_check(&ws, &command, None).await,
        };
        p.tried.push(scripted(sc, v));
        tell(&p);
    }
    let mut shots = Vec::new();
    if look_now && !p.verify.iter().any(|v| !v.ok) {
        let dir = places.shots.clone().unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        for command in &cfg.look {
            p.stage = "Taking screenshots".into();
            tell(&p);
            let v = match &lab {
                Some(l) => l.run(&format!("mkdir -p {} && {command}", crate::lab::SHOTS), &[("TESSEL_SHOTS", &l.shots_dir())]).await,
                None => run_with(&ws, command, None, &[("TESSEL_SHOTS", &dir.to_string_lossy())]).await,
            };
            if !v.ok {
                p.verify.push(v);
            }
        }
        shots = match &lab {
            Some(l) => {
                l.fetch_shots(&dir).await;
                collect_shots(&dir)
            }
            None => collect_shots(&dir),
        };
        if shots.is_empty() && !p.verify.iter().any(|v| !v.ok) {
            p.verify.push(Verified { command: cfg.look.join(" && "), ok: false, output: format!("it saved no PNG screenshots into $TESSEL_SHOTS ({})", dir.display()), ms: 0, rule: None });
        }
        p.shots = shots.iter().map(|s| s.to_string_lossy().into_owned()).collect();
        tell(&p);
    }
    if let Some(l) = &lab {
        l.close().await;
    }
    if p.verify.iter().any(|v| !v.ok) || p.tried.iter().any(|t| !t.ok) {
        return finish(p, Some("a check failed: fix that first, and the reviewers look after"));
    }
    if room.used >= 0.95 && !room.anyway {
        p.skipped = true;
        return finish(p, Some(&format!("reviewers skipped: your usage limit is at {:.0}%", room.used * 100.0)));
    }

    if !shots.is_empty() {
        p.stage = format!("Looking at {} screens", shots.len());
        tell(&p);
        let look_rules: Vec<&Rule> = applying.iter().filter(|r| r.check == Check::Look).collect();
        let model = match cfg.model.as_str() {
            "auto" | "haiku" | "sonnet" | "opus" if room.used >= 0.8 && !room.anyway => "haiku".to_owned(),
            "auto" | "haiku" => "sonnet".to_owned(),
            other => other.to_owned(),
        };
        let mut job = tessel_agent::backend::Job::review(look_prompt(&ask, &change, &shots, &look_rules), ws.clone(), &model);
        job.tools = "Read".into();
        job.system = "You judge how software looks from screenshots, like a careful designer. Be precise and brief.".into();
        job.dirs = places.shots.iter().cloned().collect();
        let names: Vec<String> = shots.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        let ids: Vec<String> = look_rules.iter().map(|r| r.id.clone()).collect();
        let start = Instant::now();
        let got = backend.job(job).await;
        let ms = start.elapsed().as_millis() as u64;
        match got {
            Ok(r) => {
                p.cost_usd += r.cost_usd;
                match parse_findings(&["look".into()], &r.text, &names, &ids) {
                    Ok(f) => {
                        p.findings.extend(f.into_iter().map(|mut f| {
                            f.confirmed = Some(true);
                            f
                        }));
                        p.reviewers.push(Reviewer { lens: "look".into(), error: None, cost_usd: r.cost_usd, ms });
                    }
                    Err(e) => p.reviewers.push(Reviewer { lens: "look".into(), error: Some(e), cost_usd: r.cost_usd, ms }),
                }
            }
            Err(e) => p.reviewers.push(Reviewer { lens: "look".into(), error: Some(format!("{e:#}")), cost_usd: 0.0, ms }),
        }
        tell(&p);
        if p.findings.iter().any(|f| f.lens == "look" && f.severity == Severity::Block) {
            return finish(p, Some("it looks broken: fix that first, and the reviewers look after"));
        }
    }

    let testing: Vec<&Scenario> = picked.iter().filter(|s| s.run.is_none()).take(cfg.testers).collect();
    if !testing.is_empty() {
        let model = match cfg.model.as_str() {
            _ if room.used >= 0.8 && !room.anyway => "haiku".to_owned(),
            "auto" | "haiku" => "sonnet".to_owned(),
            other => other.to_owned(),
        };
        for sc in testing {
            p.stage = format!("Trying {}", sc.title);
            tell(&p);
            let t = try_scenario(&backend, &ws, &root, &places, sc, &ask, &change, &model).await;
            p.cost_usd += t.cost_usd;
            p.tried.push(t);
            tell(&p);
        }
        if p.tried.iter().any(|t| !t.ok && !t.broken) {
            return finish(p, Some("a scenario failed: fix that first, and the reviewers look after"));
        }
    }

    let judged = applying.iter().any(|r| r.check == Check::Judge);
    let calls = panel(&cfg, &change, judged);
    let model = match cfg.model.as_str() {
        "haiku" | "sonnet" | "opus" | "auto" => model_for(&cfg.model, &calls, room).to_owned(),
        custom => custom.to_owned(),
    };
    p.stage = match calls.len() {
        1 => "Reviewing".to_owned(),
        n => format!("Reviewing with {n} reviewers"),
    };
    tell(&p);
    let order: Vec<&str> = calls.iter().flatten().map(String::as_str).collect();
    let order: Vec<String> = order.into_iter().map(str::to_owned).collect();
    let rule_ids: Vec<String> = applying.iter().map(|r| r.id.clone()).collect();
    let mut reviewers = tokio::task::JoinSet::new();
    for lenses in calls {
        let rule_refs: Vec<&Rule> = applying
            .iter()
            .filter(|r| lenses.iter().any(|l| l == "rules") || (r.check == Check::Judge && r.severity == Severity::Block && lenses.iter().any(|l| l == "security")))
            .collect();
        let know_refs: Vec<&Knowledge> = knowledge.iter().collect();
        let prompt = review_prompt(&lenses, &ask, &change, &rule_refs, &know_refs, &book.guidance);
        let (b, cwd, model, changed, ids) = (backend.clone(), ws.clone(), model.clone(), change.paths.clone(), rule_ids.clone());
        reviewers.spawn(async move {
            let start = Instant::now();
            let got = b.review(prompt, cwd, &model).await;
            let ms = start.elapsed().as_millis() as u64;
            let lens = lenses.join(" · ");
            match got {
                Ok(r) => match parse_findings(&lenses, &r.text, &changed, &ids) {
                    Ok(f) => (Reviewer { lens, error: None, cost_usd: r.cost_usd, ms }, f),
                    Err(e) => (Reviewer { lens, error: Some(e), cost_usd: r.cost_usd, ms }, Vec::new()),
                },
                Err(e) => (Reviewer { lens, error: Some(format!("{e:#}")), cost_usd: 0.0, ms }, Vec::new()),
            }
        });
    }
    let mut found = Vec::new();
    while let Some(done) = reviewers.join_next().await {
        if let Ok((r, f)) = done {
            p.cost_usd += r.cost_usd;
            p.reviewers.push(r);
            found.extend(f);
            tell(&p);
        }
    }
    let at = |lens: &str| order.iter().position(|l| lens.starts_with(l.as_str()));
    p.reviewers.sort_by_key(|r| at(&r.lens));
    found.sort_by_key(|f| (at(&f.lens), f.file.clone(), f.line));
    let mut found = merged(found);

    let weighty: Vec<usize> = found.iter().enumerate().filter(|(_, f)| f.severity != Severity::Note).map(|(i, _)| i).collect();
    if !weighty.is_empty() {
        p.stage = format!("Checking {} findings", weighty.len());
        tell(&p);
        let start = Instant::now();
        let mut checking: Vec<Finding> = weighty.iter().map(|&i| found[i].clone()).collect();
        match backend.review(recheck_prompt(&change, &checking), ws.clone(), &model).await {
            Ok(r) => {
                apply_recheck(&mut checking, &r.text);
                for (k, &i) in weighty.iter().enumerate() {
                    found[i].confirmed = checking[k].confirmed;
                }
                p.cost_usd += r.cost_usd;
                p.reviewers.push(Reviewer { lens: "recheck".into(), error: None, cost_usd: r.cost_usd, ms: start.elapsed().as_millis() as u64 });
            }
            Err(e) => p.reviewers.push(Reviewer { lens: "recheck".into(), error: Some(format!("{e:#}")), cost_usd: 0.0, ms: start.elapsed().as_millis() as u64 }),
        }
    }
    found.sort_by_key(|f| (f.confirmed == Some(false), f.severity != Severity::Block, f.severity != Severity::Warn));
    p.findings.extend(found);
    finish(p, None)
}

pub fn panel(cfg: &crate::rules::Config, c: &Change, judged: bool) -> Vec<Vec<String>> {
    let own = |l: &[&str]| l.iter().map(|s| (*s).to_owned()).collect::<Vec<String>>();
    if let Some(lenses) = &cfg.lenses {
        return lenses.iter().map(|l| vec![l.clone()]).collect();
    }
    let risky = crate::rules::applies(&cfg.risk, &c.paths) && !cfg.risk.is_empty();
    if risky || c.changed > 400 {
        return crate::rules::LENSES.iter().map(|l| vec![(*l).to_owned()]).collect();
    }
    let with_rules = |mut l: Vec<String>| {
        if judged {
            l.push("rules".into());
        }
        l
    };
    if c.changed > 60 || c.paths.len() > 4 {
        return vec![with_rules(own(&["correctness", "edge-cases", "security", "scope"]))];
    }
    vec![with_rules(own(&["correctness", "scope"]))]
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -10,3 +10,4 @@ fn x()\n context\n-old\n+let key = \"AKIAABCDEFGHIJKLMNOP\";\n+    it.only(\"x\")\n context\n";

    #[test]
    fn added_lines_keep_their_line_in_the_new_file() {
        let a = added_lines(DIFF);
        assert_eq!(a[0], ("src/a.rs".into(), 11, "let key = \"AKIAABCDEFGHIJKLMNOP\";".into()));
        assert_eq!(a[1].1, 12);
    }

    #[test]
    fn match_rules_find_what_they_forbid_and_miss_what_they_require() {
        let c = Change { diff: DIFF.into(), added: added_lines(DIFF), paths: vec!["src/a.rs".into(), "Cargo.toml".into()], subjects: vec!["feat: fine".into(), "Fixed stuff".into()], ..Default::default() };
        let mut rules = crate::rules::starter();
        rules.push(crate::rules::commit_style(&["feat: a", "fix: b", "feat: c", "fix: d", "chore: e"].map(String::from)).unwrap());
        let f = matched(&rules, &c);
        let ids: Vec<(&str, Option<u32>)> = f.iter().map(|f| (f.rule.as_deref().unwrap(), f.line)).collect();
        assert!(ids.contains(&("no-secrets", Some(11))));
        assert!(ids.contains(&("no-skipped-tests", Some(12))));
        assert!(ids.contains(&("dependencies-changed", None)));
        assert!(f.iter().any(|f| f.rule.as_deref() == Some("commit-style") && f.why.contains("Fixed stuff")));
        assert!(!f.iter().any(|f| f.why.contains("feat: fine")));
    }

    #[test]
    fn a_reviewer_s_findings_need_a_file_and_a_known_rule() {
        let text = "I looked.\n```json\n[{\"file\":\"src/a.rs\",\"line\":11,\"severity\":\"block\",\"rule\":\"k8s\",\"title\":\"Direct update\",\"why\":\"calls Update\"},\n{\"title\":\"vague worry\",\"severity\":\"block\"},\n{\"file\":\"src/a.rs\",\"severity\":\"warn\",\"rule\":\"made-up\",\"title\":\"x\"}]\n```";
        let f = parse_findings(&["rules".into()], text, &["src/a.rs".into()], &["k8s".into()]).unwrap();
        assert_eq!(f.len(), 2, "no file, no finding");
        assert_eq!(f[0].rule.as_deref(), Some("k8s"));
        assert_eq!(f[1].rule, None, "a rule that doesn't exist isn't cited");
        assert!(parse_findings(&["x".into()], "all good!", &[], &[]).is_err());
        assert!(parse_findings(&["x".into()], "nothing\n[]", &[], &[]).unwrap().is_empty());
    }

    #[test]
    fn a_recheck_drops_what_it_refutes_and_the_verdict_follows() {
        let f = |sev: Severity| Finding { lens: "correctness".into(), rule: None, file: Some("a".into()), line: Some(1), severity: sev, title: "t".into(), why: String::new(), confirmed: None };
        let mut p = Proof { findings: vec![f(Severity::Block), f(Severity::Warn)], ..Default::default() };
        assert_eq!(verdict(&p), Verdict::Failed);
        apply_recheck(&mut p.findings, "```json\n[{\"n\":0,\"real\":false},{\"n\":1,\"real\":true}]\n```");
        assert_eq!(verdict(&p), Verdict::Concerns, "the refuted block no longer counts");
        let mut q = Proof { findings: vec![f(Severity::Block)], ..Default::default() };
        apply_recheck(&mut q.findings, "```json\n[{\"n\":0,\"real\":true,\"severity\":\"warn\"}]\n```");
        assert_eq!((q.findings[0].severity.clone(), verdict(&q)), (Severity::Warn, Verdict::Concerns), "true but smaller is lowered, not dropped");
        apply_recheck(&mut q.findings, "```json\n[{\"n\":0,\"real\":true,\"severity\":\"block\"}]\n```");
        assert_eq!(q.findings[0].severity, Severity::Warn, "a recheck never raises one");
        p.findings[1].confirmed = Some(false);
        assert_eq!(verdict(&p), Verdict::Proven);
        p.verify.push(Verified { command: "cargo test".into(), ok: false, output: String::new(), ms: 1, rule: None });
        assert_eq!(verdict(&p), Verdict::Failed, "a failing check fails it");
    }

    #[test]
    fn the_same_problem_seen_by_several_reviewers_is_one_finding() {
        let f = |lens: &str, line: u32, rule: Option<&str>, sev: Severity| Finding { lens: lens.into(), rule: rule.map(str::to_owned), file: Some("k8s/deploy.py".into()), line: Some(line), severity: sev, title: format!("{lens} says"), why: String::new(), confirmed: None };
        let m = merged(vec![
            f("rules", 10, Some("k8s"), Severity::Warn),
            f("security", 10, Some("k8s"), Severity::Block),
            f("correctness", 9, Some("k8s"), Severity::Block),
            f("correctness", 9, None, Severity::Warn),
            f("edge-cases", 9, None, Severity::Warn),
            f("scope", 30, None, Severity::Note),
        ]);
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].lens.as_str(), m[0].severity.clone()), ("rules · security · correctness", Severity::Block), "the strongest word wins");
        assert_eq!(m[1].lens, "correctness · edge-cases");
    }

    #[test]
    fn the_panel_grows_with_the_change_and_its_risk() {
        let cfg = |lenses: Option<Vec<String>>| crate::rules::Config { verify: vec![], model: "auto".into(), lenses, risk: crate::rules::RISK.iter().map(|s| (*s).to_owned()).collect(), starter: true, gate: true, auto: false, look: vec![], look_paths: vec![], scenarios: crate::rules::Scenarios::Matching, testers: 3, lab: None, lab_setup: vec![] };
        let change = |changed: usize, paths: &[&str]| Change { changed, paths: paths.iter().map(|p| (*p).to_owned()).collect(), ..Default::default() };
        let small = change(12, &["src/list.rs"]);
        assert_eq!(panel(&cfg(None), &small, false), vec![vec!["correctness".to_owned(), "scope".to_owned()]], "a small change is one reviewer");
        assert_eq!(panel(&cfg(None), &small, true)[0].last().map(String::as_str), Some("rules"), "the house rules ride along when one applies");
        assert_eq!(panel(&cfg(None), &change(120, &["src/list.rs"]), false), vec![vec!["correctness".to_owned(), "edge-cases".to_owned(), "security".to_owned(), "scope".to_owned()]], "a medium change is still one reviewer, looking further");
        assert_eq!(panel(&cfg(None), &change(5, &["src/auth/session.rs"]), false).len(), 5, "anything near auth gets everyone");
        assert_eq!(panel(&cfg(None), &change(900, &["src/list.rs"]), false).len(), 5);
        assert_eq!(panel(&cfg(Some(vec!["security".into()])), &small, false), vec![vec!["security".to_owned()]], "a repository that names its lenses gets those");
        let both = vec!["correctness".to_owned(), "scope".to_owned()];
        let f = parse_findings(&both, "```json\n[{\"lens\":\"scope\",\"file\":\"a\",\"title\":\"x\"},{\"lens\":\"made-up\",\"file\":\"a\",\"title\":\"y\"}]\n```", &[], &[]).unwrap();
        assert_eq!((f[0].lens.as_str(), f[1].lens.as_str()), ("scope", "correctness"));
    }

    #[test]
    fn the_reviewer_s_model_follows_the_change_and_how_much_is_left() {
        let one = vec![vec!["correctness".to_owned(), "scope".to_owned()]];
        let wide = vec![vec!["correctness".to_owned(), "edge-cases".to_owned(), "security".to_owned(), "scope".to_owned()]];
        let full: Vec<Vec<String>> = crate::rules::LENSES.iter().map(|l| vec![(*l).to_owned()]).collect();
        let free = Room::default();
        assert_eq!(model_for("auto", &one, free), "haiku", "a small change is a small model's job");
        assert_eq!(model_for("auto", &wide, free), "sonnet");
        assert_eq!(model_for("auto", &full, free), "sonnet");
        assert_eq!(model_for("opus", &one, free), "opus", "a repository that asks for one gets it");
        let tight = Room { used: 0.85, anyway: false };
        assert_eq!(model_for("auto", &full, tight), "haiku", "near the limit everything is the small model");
        assert_eq!(model_for("auto", &full, Room { anyway: true, ..tight }), "sonnet");
        let w = |name: &str, used: f64, resets_at: Option<i64>| tessel_proto::UsageWindow { name: name.into(), used, resets_at, status: String::new() };
        assert_eq!(room(&[w("five_hour", 0.4, Some(100)), w("seven_day", 0.9, Some(50))], 60, false).used, 0.4, "a window that has started over doesn't count");
    }

    #[tokio::test]
    async fn a_change_is_only_what_was_done_since_its_workspace_started_even_when_the_local_main_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        let sh = |at: &Path, args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(at).args(args).output().unwrap();
            assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        };
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(&root, a);
        }
        std::fs::write(root.join("a.txt"), "one\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "start"]);
        sh(&root, &["checkout", "-q", "-b", "feature"]);
        for n in 0..3 {
            std::fs::write(root.join(format!("old{n}.txt")), "AKIAABCDEFGHIJKLMNOP\n").unwrap();
            sh(&root, &["add", "-A"]);
            sh(&root, &["commit", "-q", "-m", &format!("Someone else's work {n}")]);
        }
        let spec = tessel_proto::WorktreeSpec { base: Some("feature".into()), name: Some("task".into()) };
        let made = crate::worktree::create(root.to_str().unwrap(), &spec, &dir.path().join("wts"), "task").await.unwrap();
        let ws = PathBuf::from(&made.path);
        std::fs::write(ws.join("mine.txt"), "mine\n").unwrap();
        sh(&ws, &["add", "-A"]);
        sh(&ws, &["commit", "-q", "-m", "fix: mine"]);
        let c = change(&ws).await.unwrap();
        assert_eq!(c.paths, vec!["mine.txt".to_owned()], "not the 3 commits the stale main is missing");
        assert_eq!(c.subjects, vec!["fix: mine".to_owned()]);
    }

    #[tokio::test]
    async fn what_a_tester_changes_is_put_back_and_the_agent_s_own_work_is_kept() {
        let dir = tempfile::tempdir().unwrap();
        let ws = dir.path();
        let sh = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(ws).args(args).status().unwrap().success());
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(a);
        }
        std::fs::write(ws.join("a.txt"), "one\n").unwrap();
        std::fs::write(ws.join("b.txt"), "b\n").unwrap();
        sh(&["add", "-A"]);
        sh(&["commit", "-q", "-m", "start"]);
        std::fs::write(ws.join("a.txt"), "agent's work\n").unwrap();
        std::fs::write(ws.join("new.txt"), "agent's new file\n").unwrap();
        let before = remember(ws).await;
        std::fs::write(ws.join("a.txt"), "tester scribbled\n").unwrap();
        std::fs::write(ws.join("b.txt"), "tester scribbled\n").unwrap();
        std::fs::write(ws.join("junk.log"), "tester's\n").unwrap();
        put_back(ws, &before).await;
        assert_eq!(std::fs::read_to_string(ws.join("a.txt")).unwrap(), "agent's work\n");
        assert_eq!(std::fs::read_to_string(ws.join("b.txt")).unwrap(), "b\n");
        assert_eq!(std::fs::read_to_string(ws.join("new.txt")).unwrap(), "agent's new file\n");
        assert!(!ws.join("junk.log").exists());
    }

    #[tokio::test]
    async fn a_worktree_of_a_rust_repository_builds_into_the_main_checkout_s_target_not_a_new_one() {
        let dir = tempfile::tempdir().unwrap();
        let (root, ws) = (dir.path().join("root"), dir.path().join("ws"));
        std::fs::create_dir_all(&root).unwrap();
        let sh = |at: &Path, args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(at).args(args).status().unwrap().success());
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(&root, a);
        }
        std::fs::write(root.join("Cargo.toml"), "[workspace]\n").unwrap();
        sh(&root, &["add", "-A"]);
        sh(&root, &["commit", "-q", "-m", "start"]);
        sh(&root, &["worktree", "add", "-q", "-b", "t", ws.to_str().unwrap()]);
        let target = std::fs::canonicalize(&root).unwrap().join("target");
        let check = format!("test \"$CARGO_TARGET_DIR\" = '{}'", target.display());
        assert!(run_check(&ws, &check, None).await.ok, "a task's worktree shares it");
        assert!(run_check(&root, "test -z \"$CARGO_TARGET_DIR\"", None).await.ok, "the main checkout keeps its own as usual");
        std::fs::create_dir_all(ws.join(".tessel")).unwrap();
        std::fs::write(ws.join(".tessel/settings.toml"), "[factory]\nshare_build = false\n").unwrap();
        assert!(run_check(&ws, "test -z \"$CARGO_TARGET_DIR\"", None).await.ok, "a repository can say no");
    }

    #[test]
    fn free_scripts_always_run_and_testers_only_where_the_change_reaches() {
        let sc = |id: &str, paths: &[&str], run: Option<&str>| Scenario { id: id.into(), title: id.into(), path: String::new(), paths: paths.iter().map(|p| (*p).to_owned()).collect(), run: run.map(str::to_owned), body: "x".into(), holdout: false };
        let all = vec![sc("script", &[], Some("true")), sc("checkout", &["shop/**"], None), sc("anything", &[], None), sc("admin", &["admin/**"], None)];
        let mut cfg = crate::rules::config(Path::new("/nowhere"), Path::new("/nowhere"));
        let ids = |cfg: &crate::rules::Config, c: &Change, risky: bool| pick_scenarios(cfg, &all, c, risky).into_iter().map(|s| s.id).collect::<Vec<_>>();
        let small = Change { changed: 10, paths: vec!["shop/cart.rs".into()], ..Default::default() };
        assert_eq!(ids(&cfg, &small, false), vec!["script", "checkout"]);
        assert_eq!(ids(&cfg, &small, true), vec!["script", "checkout", "anything"], "near auth, the general ones too");
        assert_eq!(ids(&cfg, &Change { changed: 200, ..small.clone() }, false), vec!["script", "checkout", "anything"]);
        cfg.scenarios = crate::rules::Scenarios::Never;
        assert!(ids(&cfg, &small, true).is_empty());
        cfg.scenarios = crate::rules::Scenarios::Always;
        assert_eq!(ids(&cfg, &small, false).len(), 4);
        cfg.look = vec!["shots".into()];
        cfg.look_paths = vec!["ui/**".into()];
        assert!(!looks(&cfg, &small) && looks(&cfg, &Change { paths: vec!["ui/board.rs".into()], ..small }));
    }

    #[test]
    fn a_tester_s_answer_is_passed_failed_or_couldn_t_tell() {
        let sc = Scenario { id: "pay".into(), title: "Pay".into(), path: String::new(), paths: vec![], run: None, body: String::new(), holdout: true };
        let t = parse_tried(&sc, "did it\n```json\n{\"passed\": false, \"said\": \"500\"}\n```", 0.02, 9);
        assert!(!t.ok && !t.broken && t.said == "500" && t.holdout);
        assert!(parse_tried(&sc, "```json\n{\"passed\": true, \"said\": \"ok\"}\n```", 0.0, 1).ok);
        let unsure = parse_tried(&sc, "```json\n{\"passed\": null, \"said\": \"no network\"}\n```", 0.0, 1);
        assert!(!unsure.ok && unsure.broken);
        let p = Proof { tried: vec![unsure], ..Default::default() };
        assert_eq!(verdict(&p), Verdict::Concerns, "couldn't tell isn't a failure, but it isn't proven either");
        assert!(tester_prompt(&sc, &Ask { goal: "add coupons".into(), said: vec![] }, &Change::default()).contains("Don't edit, commit or delete"));
    }

    #[test]
    fn with_comments_off_an_added_comment_is_caught_in_each_language_and_code_is_not() {
        let diff = "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,1 +1,5 @@\n+// explains it\n+#[derive(Debug)]\n+let url = \"https://x\";\n+    /* block */\n+fn f() {}\n\
diff --git a/run.sh b/run.sh\n--- a/run.sh\n+++ b/run.sh\n@@ -1,1 +1,3 @@\n+#!/bin/sh\n+# start it\n+echo hi\n\
diff --git a/q.sql b/q.sql\n--- a/q.sql\n+++ b/q.sql\n@@ -1,1 +1,2 @@\n+-- the users\n+select 1;\n\
diff --git a/README.md b/README.md\n--- a/README.md\n+++ b/README.md\n@@ -1,1 +1,1 @@\n+# Title\n";
        let c = Change { diff: diff.into(), added: added_lines(diff), paths: vec!["src/a.rs".into(), "run.sh".into(), "q.sql".into(), "README.md".into()], ..Default::default() };
        let found: Vec<(String, u32)> = matched(&crate::rules::no_comments(), &c).into_iter().map(|f| (f.file.unwrap(), f.line.unwrap())).collect();
        assert_eq!(found, vec![("src/a.rs".into(), 1), ("src/a.rs".into(), 4), ("run.sh".into(), 2), ("q.sql".into(), 1)], "not attributes, urls, shebangs or markdown headings");
    }

    #[test]
    fn long_output_keeps_the_errors_and_the_end() {
        let mut out = String::new();
        for i in 0..500 {
            out.push_str(&format!("compiling crate {i}\n"));
        }
        out.push_str("error[E0308]: mismatched types\n");
        for i in 0..100 {
            out.push_str(&format!("note {i}\n"));
        }
        let t = trimmed(&out);
        assert!(t.contains("error[E0308]") && t.contains("note 99") && !t.contains("compiling crate 3\n"));
    }
}
