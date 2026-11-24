use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tessel_agent::Backend;
use tessel_proto::proof::{Check, Finding, Knowledge, Proof, Reviewer, Rule, Rulebook, Severity, Verdict, Verified, Within};

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

pub async fn change(ws: &Path) -> Result<Change, String> {
    let main = crate::ship::main_branch(ws).await;
    let head = git(ws, &["rev-parse", "HEAD"]).await?;
    let base = git(ws, &["merge-base", "HEAD", &main]).await.unwrap_or_else(|_| head.clone());
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
    let start = Instant::now();
    let mut cmd = tokio::process::Command::new("sh");
    cmd.args(["-c", command]).current_dir(ws).env("PYTHONDONTWRITEBYTECODE", "1").stdin(std::process::Stdio::null()).kill_on_drop(true);
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
    if p.verify.iter().any(|v| !v.ok) || p.standing().any(|f| f.severity == Severity::Block) {
        Verdict::Failed
    } else if p.standing().any(|f| f.severity == Severity::Warn) {
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

pub async fn prove(backend: Arc<dyn Backend>, ws: PathBuf, root: PathBuf, holdouts: Option<PathBuf>, ask: Ask, room: Room, now: impl Fn() -> u64 + Send + Sync + 'static, tell: impl Fn(&Proof) + Send + Sync + 'static) -> Proof {
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
    let book: Rulebook = crate::rules::load_with(&ws, &root, holdouts.as_deref());
    let cfg = crate::rules::config(&ws, &root);
    p.risky = !cfg.risk.is_empty() && crate::rules::applies(&cfg.risk, &change.paths);
    let mut rules: Vec<Rule> = book.rules.into_iter().filter(|r| r.check != Check::Unknown).collect();
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

    let mut commands: Vec<(String, Option<String>)> = cfg.verify.iter().map(|c| (c.clone(), None)).collect();
    commands.extend(applying.iter().filter_map(|r| match &r.check {
        Check::Run { command } => Some((command.clone(), Some(r.id.clone()))),
        _ => None,
    }));
    for (command, rule) in commands {
        p.stage = format!("Running {command}");
        tell(&p);
        p.verify.push(run_check(&ws, &command, rule).await);
        tell(&p);
    }
    if p.verify.iter().any(|v| !v.ok) {
        return finish(p, Some("a check failed: fix that first, and the reviewers look after"));
    }

    let judged = applying.iter().any(|r| r.check == Check::Judge);
    let calls = panel(&cfg, &change, judged);
    if room.used >= 0.95 && !room.anyway {
        p.skipped = true;
        return finish(p, Some(&format!("reviewers skipped: your usage limit is at {:.0}%", room.used * 100.0)));
    }
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
        let cfg = |lenses: Option<Vec<String>>| crate::rules::Config { verify: vec![], model: "auto".into(), lenses, risk: crate::rules::RISK.iter().map(|s| (*s).to_owned()).collect(), starter: true, gate: true, auto: false };
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
