use std::collections::BTreeMap;
use std::path::Path;
use tessel_proto::proof::{Proof, Verdict};
use tessel_proto::task::{Risk, Task, TaskState};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Land {
    Main,
    PullRequest,
    Branch(String),
    Keep,
}

pub fn parse_land(s: &str) -> Land {
    match s.trim() {
        "pr" | "pull-request" => Land::PullRequest,
        "keep" => Land::Keep,
        other => match other.strip_prefix("branch:").map(str::trim).filter(|b| !b.is_empty()) {
            Some(b) => Land::Branch(b.to_owned()),
            None => Land::Main,
        },
    }
}

#[derive(Clone, Debug)]
pub struct Factory {
    pub model: Option<String>,
    pub land: Option<Land>,
    pub auto_land: bool,
    pub fix_rounds: u32,
    pub checkup_ready: bool,
    pub issues: Option<String>,
}

pub fn factory(workspace: &Path, root: &Path) -> Factory {
    let (t, _) = crate::setup::merged(workspace, root);
    let f = t.get("factory").and_then(toml::Value::as_table);
    let get = |k: &str| f.and_then(|f| f.get(k));
    Factory {
        model: get("model").and_then(toml::Value::as_str).map(str::to_owned),
        land: get("land").and_then(toml::Value::as_str).map(parse_land),
        auto_land: get("auto_land").and_then(toml::Value::as_bool).unwrap_or(false),
        fix_rounds: get("fix_rounds").and_then(toml::Value::as_integer).map_or(3, |n| n.clamp(0, 5) as u32),
        checkup_ready: get("checkup").and_then(toml::Value::as_str) != Some("draft"),
        issues: get("issues").and_then(toml::Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned),
    }
}

pub fn new_id(taken: impl Fn(&str) -> bool) -> String {
    loop {
        let u = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("t{}", &u[..5]);
        if !taken(&id) {
            return id;
        }
    }
}

pub fn repo_root(path: &Path) -> std::path::PathBuf {
    let out = std::process::Command::new("git").arg("-C").arg(path).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]).output();
    match out {
        Ok(o) if o.status.success() => {
            let common = std::path::PathBuf::from(String::from_utf8_lossy(&o.stdout).trim());
            if common.file_name().is_some_and(|n| n == ".git") { common.parent().map(Path::to_path_buf).unwrap_or(common) } else { common }
        }
        _ => path.to_path_buf(),
    }
}

pub fn ready(tasks: &BTreeMap<String, Task>) -> Vec<String> {
    let mut out: Vec<&Task> = tasks
        .values()
        .filter(|t| t.state == TaskState::Ready && t.deps.iter().all(|d| tasks.get(d).is_some_and(|x| matches!(x.state, TaskState::Landed | TaskState::Dropped))))
        .collect();
    out.sort_by_key(|t| t.created_ms);
    out.into_iter().map(|t| t.id.clone()).collect()
}

pub fn blocked_by<'a>(task: &Task, tasks: &'a BTreeMap<String, Task>) -> Vec<&'a Task> {
    task.deps.iter().filter_map(|d| tasks.get(d)).filter(|x| !matches!(x.state, TaskState::Landed | TaskState::Dropped)).collect()
}

pub fn risk(p: &Proof) -> Risk {
    if p.risky || p.verdict == Verdict::Failed {
        Risk::High
    } else if p.verdict != Verdict::Proven || p.files > 10 || p.findings.iter().any(|f| f.confirmed != Some(false) && f.severity != tessel_proto::proof::Severity::Note) {
        Risk::Medium
    } else {
        Risk::Low
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Next {
    Fix(String),
    Review(Option<String>),
    Land,
}

pub fn after_proof(task: &Task, p: &Proof, f: &Factory) -> Next {
    match p.verdict {
        Verdict::Failed if task.attempts < f.fix_rounds && !p.skipped => Next::Fix(format!(
            "Checking your change found problems. Fix the ones that are real and say in a line which aren't and why, commit, then stop: it's checked again.\n\n{}",
            p.report()
        )),
        Verdict::Failed => Next::Review(Some(format!("still failing after {} fix rounds", task.attempts))),
        Verdict::Broken => Next::Review(Some(p.problem.clone().unwrap_or_else(|| "it couldn't be checked".into()))),
        _ => Next::Land,
    }
}

pub fn mode<'a>(autonomy: &'a str, bother: &str) -> &'a str {
    match autonomy {
        "custom" if bother.trim().is_empty() => "agent",
        "me" | "agent" | "ui" | "full" | "custom" => autonomy,
        _ => "agent",
    }
}

pub fn final_review_prompt(t: &Task, report: &str, diff: &str, autonomy: &str, bother: &str) -> String {
    let mode = mode(autonomy, bother);
    let full = mode == "full";
    let mut p = String::from(
        "You are the final reviewer of a change an agent made, deciding for the person who asked for it so they don't have to look. \
Land it if it does what was asked, completely, and nothing in it is wrong or unsafe. \
Send it back with \"change\" when something specific is wrong or missing that the agent can fix: say exactly what. ",
    );
    const CHOOSE: &str = "When the task leaves a product or design choice open, pick the reading that best fits the task and the product as it is, land it, and say in \"why\" which choice you made. ";
    match mode {
        "full" => {
            p.push_str("Nobody will answer questions: the person runs this hands-off, so never answer \"ask\". ");
            p.push_str(CHOOSE);
        }
        "ui" => {
            p.push_str("Ask the person with \"ask\" only for a choice about how something looks or reads on screen, and set \"visual\" to true when you do. Decide everything else yourself, as nobody else will look. ");
            p.push_str(CHOOSE);
        }
        "custom" => p.push_str(&format!("Ask the person only when this applies: {}. Otherwise decide yourself, as nobody else will look. To ask, answer \"ask\". ", bother.trim().trim_end_matches('.'))),
        _ => p.push_str("Ask the person with \"ask\" only for a decision only they can make: a product or design choice, a risk you can't judge, or a task that's unclear. "),
    }
    if !full {
        p.push_str(
            "When you ask, give 2 or 3 concrete options they can pick between, each with a short label of a few words and what it does. \
Set \"visual\" to true only when the choice is about how something looks or reads on screen and seeing each option would settle it. ",
        );
    }
    p.push_str("You can read the repository (Read, Grep, Glob).\n");
    p.push_str(&format!("\n## The task\n{}\n", t.title));
    if !t.spec.trim().is_empty() {
        p.push_str(&format!("\n{}\n", t.spec.trim()));
    }
    for n in t.notes.iter().filter(|n| n.by == "you") {
        p.push_str(&format!("\nAlso asked: {}\n", n.text));
    }
    if let Some(s) = &t.summary {
        p.push_str(&format!("\n## What the agent says it did\n{s}\n"));
    }
    p.push_str(&format!("\n## What the checks found\n{report}\n"));
    let diff: String = diff.chars().take(100_000).collect();
    p.push_str(&format!("\n## The change\n```diff\n{diff}\n```\n"));
    if full {
        p.push_str("\n## Answer\nEnd with one ```json block and nothing after it: {\"decision\": \"land\" | \"change\", \"why\": \"one plain sentence the person reads\", \"change\": \"what to change, when you send it back\"}.\n");
    } else {
        p.push_str("\n## Answer\nEnd with one ```json block and nothing after it: {\"decision\": \"land\" | \"change\" | \"ask\", \"why\": \"one plain sentence the person reads\", \"change\": \"what to change, when you send it back\", \"options\": [{\"label\": \"a few words\", \"what\": \"what this option does, concretely\"}], \"visual\": true | false}. Give options and visual only when you ask.\n");
    }
    p
}

#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Land(String),
    Change(String, String),
    Ask { why: String, options: Vec<(String, String)>, visual: bool },
}

pub fn read_decision(text: &str) -> Decision {
    let v = crate::proof::json_tail(text).unwrap_or_default();
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).map(str::trim).filter(|x| !x.is_empty()).map(str::to_owned);
    let why = s("why").unwrap_or_else(|| "the reviewer didn't say why".into());
    match s("decision").as_deref() {
        Some("land") => Decision::Land(why),
        Some("change") => Decision::Change(why.clone(), s("change").unwrap_or(why)),
        _ => {
            let options: Vec<(String, String)> = v
                .get("options")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    let f = |k: &str| o.get(k).and_then(serde_json::Value::as_str).map(str::trim).unwrap_or_default().to_owned();
                    let (label, what) = (f("label"), f("what"));
                    let label = if label.is_empty() { what.chars().take(40).collect() } else { label };
                    (!label.is_empty()).then_some((label, what))
                })
                .take(3)
                .collect();
            let visual = !options.is_empty() && v.get("visual").and_then(serde_json::Value::as_bool).unwrap_or(false);
            Decision::Ask { why, options, visual }
        }
    }
}

pub fn option_prompt(base: &str, label: &str, what: &str) -> String {
    format!(
        "The final review has a question for the person, and they'll pick between options by seeing each one. Make this one so they can see it.\n\n\
Start from commit {base}: run `git reset --hard {base}` in this workspace first (it's your own worktree, so that's safe). \
Then make this option, and only this: {label}: {what}\n\n\
Commit it, then stop. Don't run the checks: it's screenshotted next."
    )
}

pub fn plan_prompt(t: &Task) -> String {
    let mut p = format!(
        "Plan this task, don't do it: split it into as many small tasks as it takes (usually 2 to 8, at most 15), each making one change that can be checked on its own. If it's already a list of separate things, file one task per item. Read only what you need to plan well. \
File them in order with tessel_task (op add, ready: true, and after: the ids of tasks each one needs done first), each with a title and a spec that says what done looks like. \
Don't change any files. Then stop with one line listing what you filed.\n\n{}",
        t.title
    );
    if !t.spec.trim().is_empty() {
        p.push_str("\n\n");
        p.push_str(t.spec.trim());
    }
    p
}

fn attachment_dir(home: &Path, id: &str) -> std::path::PathBuf {
    home.join("tasks").join(id)
}

pub fn save_attachments(home: &Path, id: &str, attachments: &[tessel_proto::Attachment]) -> std::io::Result<Vec<String>> {
    use base64::Engine as _;
    let dir = attachment_dir(home, id);
    std::fs::create_dir_all(&dir)?;
    let mut names = Vec::new();
    for (n, a) in attachments.iter().enumerate() {
        let base = Path::new(&a.name).file_name().map(|f| f.to_string_lossy().into_owned()).filter(|f| !f.is_empty() && !f.starts_with('.')).unwrap_or_else(|| format!("file-{n}"));
        let name = if names.contains(&base) { format!("{n}-{base}") } else { base };
        let bytes = base64::engine::general_purpose::STANDARD.decode(&a.data).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        std::fs::write(dir.join(&name), bytes)?;
        std::fs::write(dir.join(format!(".{name}.type")), &a.media_type)?;
        names.push(name);
    }
    Ok(names)
}

pub fn load_attachments(home: &Path, t: &Task) -> Vec<tessel_proto::Attachment> {
    use base64::Engine as _;
    let dir = attachment_dir(home, &t.id);
    t.attachments
        .iter()
        .filter_map(|name| {
            let bytes = std::fs::read(dir.join(name)).ok()?;
            let media_type = std::fs::read_to_string(dir.join(format!(".{name}.type"))).unwrap_or_else(|_| "application/octet-stream".into());
            Some(tessel_proto::Attachment { name: name.clone(), media_type, data: base64::engine::general_purpose::STANDARD.encode(bytes), element: None })
        })
        .collect()
}

pub fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Swatch {
    pub base: String,
    pub why: String,
}

fn swatch_file(home: &Path, id: &str) -> std::path::PathBuf {
    attachment_dir(home, id).join(".swatch")
}

pub fn save_swatch(home: &Path, id: &str, s: &Swatch) {
    let _ = std::fs::create_dir_all(attachment_dir(home, id));
    if let Ok(b) = serde_json::to_vec(s) {
        let _ = std::fs::write(swatch_file(home, id), b);
    }
}

pub fn load_swatch(home: &Path, id: &str) -> Option<Swatch> {
    serde_json::from_slice(&std::fs::read(swatch_file(home, id)).ok()?).ok()
}

pub fn forget_swatch(home: &Path, id: &str) {
    let _ = std::fs::remove_file(swatch_file(home, id));
}

pub fn task_prompt(t: &Task) -> String {
    let mut p = t.title.clone();
    if !t.spec.trim().is_empty() {
        p.push_str("\n\n");
        p.push_str(t.spec.trim());
    }
    let asked: Vec<&str> = t.notes.iter().filter(|n| n.by == "you").map(|n| n.text.as_str()).collect();
    if !asked.is_empty() {
        p.push_str("\n\nAlso:\n");
        for a in asked {
            p.push_str(&format!("- {a}\n"));
        }
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::proof::{Finding, Severity};

    fn task(id: &str, state: TaskState, deps: &[&str], at: u64) -> Task {
        Task {
            id: id.into(),
            title: format!("do {id}"),
            spec: String::new(),
            repo: "/r".into(),
            state,
            deps: deps.iter().map(|d| (*d).to_owned()).collect(),
            parent: None,
            session: None,
            notes: vec![],
            plan: false,
            attempts: 0,
            proof_cost_usd: 0.0,
            budget_usd: None,
            risk: None,
            landed: None,
            pr: None,
            issue: None,
            attachments: vec![],
            land: None,
            ask: None,
            summary: None,
            changes: vec![],
            review: None,
            cleared: false,
            shots: vec![],
            options: vec![],
            building: None,
            created_ms: at,
            updated_ms: at,
        }
    }

    #[test]
    fn the_final_reviewer_lands_sends_back_or_asks_and_says_why() {
        assert_eq!(read_decision("ok\n```json\n{\"decision\": \"land\", \"why\": \"Does what was asked.\"}\n```"), Decision::Land("Does what was asked.".into()));
        assert_eq!(read_decision("```json\n{\"decision\": \"change\", \"why\": \"Misses the empty cart.\", \"change\": \"Handle an empty cart.\"}\n```"), Decision::Change("Misses the empty cart.".into(), "Handle an empty cart.".into()));
        assert!(matches!(read_decision("I'm not sure"), Decision::Ask { .. }), "anything unclear goes to the person");
        let mut t = task("a", TaskState::Proving, &[], 0);
        t.summary = Some("Added coupons.".into());
        let p = final_review_prompt(&t, "proven", "+x", "agent", "");
        assert!(p.contains("Added coupons.") && p.contains("\"decision\"") && p.contains("+x"));
        assert!(p.contains("\"ask\"") && p.contains("\"options\"") && p.contains("\"visual\""));
    }

    #[test]
    fn a_question_comes_with_at_most_three_options_and_says_whether_seeing_them_decides_it() {
        let asked = read_decision(
            "```json\n{\"decision\": \"ask\", \"why\": \"Which color for the badge?\", \"visual\": true, \"options\": [{\"label\": \"Green\", \"what\": \"Green badge\"}, {\"label\": \"Blue\", \"what\": \"Blue badge\"}, {\"label\": \"\", \"what\": \"Gray badge\"}, {\"label\": \"Red\", \"what\": \"Red badge\"}, {}]}\n```",
        );
        let Decision::Ask { why, options, visual } = asked else { panic!("{asked:?}") };
        assert_eq!(why, "Which color for the badge?");
        assert!(visual);
        assert_eq!(options, vec![("Green".to_owned(), "Green badge".to_owned()), ("Blue".to_owned(), "Blue badge".to_owned()), ("Gray badge".to_owned(), "Gray badge".to_owned())]);
        assert_eq!(read_decision("```json\n{\"decision\": \"ask\", \"why\": \"Unclear.\", \"visual\": true}\n```"), Decision::Ask { why: "Unclear.".into(), options: vec![], visual: false }, "nothing to see without options");
        assert_eq!(read_decision("```json\n{\"decision\": \"ask\", \"why\": \"Which?\", \"options\": \"pick one\"}\n```"), Decision::Ask { why: "Which?".into(), options: vec![], visual: false });
        let p = option_prompt("abc123", "Green", "Green badge");
        assert!(p.contains("git reset --hard abc123") && p.contains("Green: Green badge") && p.contains("Commit it, then stop"));
    }

    #[test]
    fn hands_off_the_reviewer_is_told_nobody_answers_and_to_choose_for_itself() {
        let t = task("a", TaskState::Proving, &[], 0);
        let p = final_review_prompt(&t, "proven", "+x", "full", "");
        assert!(p.contains("Nobody will answer questions") && p.contains("which choice you made"));
        assert!(!p.contains("\"options\""), "no options to offer when nobody picks");
    }

    #[test]
    fn the_reviewer_bothers_the_person_only_as_often_as_they_said() {
        let t = task("a", TaskState::Proving, &[], 0);
        let ui = final_review_prompt(&t, "proven", "+x", "ui", "");
        assert!(ui.contains("only for a choice about how something looks") && ui.contains("which choice you made") && ui.contains("\"options\""));
        let custom = final_review_prompt(&t, "proven", "+x", "custom", "it changes billing.");
        assert!(custom.contains("Ask the person only when this applies: it changes billing. Otherwise decide yourself, as nobody else will look."), "{custom}");
        assert_eq!(mode("custom", "  "), "agent", "a rule left empty is no rule");
        assert_eq!(mode("sometimes", ""), "agent");
        assert_eq!(mode("ui", ""), "ui");
    }

    #[test]
    fn a_planner_is_told_to_file_tasks_not_to_write_code() {
        let mut t = task("a", TaskState::Ready, &[], 0);
        t.title = "Add billing".into();
        t.spec = "Stripe, monthly plans.".into();
        let p = plan_prompt(&t);
        assert!(p.contains("don't do it") && p.contains("tessel_task") && p.contains("after:") && p.contains("Don't change any files"));
        assert!(p.ends_with("Add billing\n\nStripe, monthly plans."));
    }

    #[test]
    fn only_ready_tasks_whose_blockers_landed_are_picked_oldest_first() {
        let all: BTreeMap<String, Task> = [
            task("a", TaskState::Ready, &[], 3),
            task("b", TaskState::Ready, &["c"], 1),
            task("c", TaskState::Running, &[], 2),
            task("d", TaskState::Draft, &[], 0),
            task("e", TaskState::Ready, &["f"], 5),
            task("f", TaskState::Landed, &[], 4),
            task("g", TaskState::Ready, &["gone"], 0),
        ]
        .into_iter()
        .map(|t| (t.id.clone(), t))
        .collect();
        assert_eq!(ready(&all), vec!["a", "e"]);
        assert_eq!(blocked_by(&all["b"], &all).iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["c"]);
    }

    #[test]
    fn a_failed_proof_goes_back_to_the_agent_a_few_times_then_to_you() {
        let f = Factory { model: None, land: Some(Land::Main), auto_land: true, fix_rounds: 2, checkup_ready: true, issues: None };
        let failed = Proof { verdict: Verdict::Failed, ..Default::default() };
        let mut t = task("a", TaskState::Proving, &[], 0);
        assert!(matches!(after_proof(&t, &failed, &f), Next::Fix(m) if m.contains("Fix the ones that are real")));
        t.attempts = 2;
        assert!(matches!(after_proof(&t, &failed, &f), Next::Review(Some(_))));
        let skipped = Proof { skipped: true, ..failed.clone() };
        t.attempts = 0;
        assert!(matches!(after_proof(&t, &skipped, &f), Next::Review(_)), "a proof that skipped its reviewers isn't worth another round");
    }

    #[test]
    fn only_a_small_clean_proven_change_lands_by_itself_and_only_when_asked_to() {
        let f = Factory { model: None, land: Some(Land::Main), auto_land: true, fix_rounds: 2, checkup_ready: true, issues: None };
        let t = task("a", TaskState::Proving, &[], 0);
        let clean = Proof { verdict: Verdict::Proven, files: 2, ..Default::default() };
        assert_eq!(risk(&clean), Risk::Low);
        assert_eq!(after_proof(&t, &clean, &f), Next::Land, "a passing proof goes on to the final review");
        let near_auth = Proof { risky: true, ..clean.clone() };
        assert_eq!(risk(&near_auth), Risk::High);
        let warned = Proof { findings: vec![Finding { lens: "scope".into(), rule: None, file: None, line: None, severity: Severity::Warn, title: "x".into(), why: String::new(), confirmed: None }], ..clean.clone() };
        assert_eq!(risk(&warned), Risk::Medium);
        assert_eq!(risk(&Proof { files: 30, ..clean }), Risk::Medium);
    }

    #[test]
    fn a_task_s_attachments_are_kept_beside_it_and_come_back_whole() {
        use base64::Engine as _;
        let home = tempfile::tempdir().unwrap();
        let a = |name: &str, bytes: &[u8]| tessel_proto::Attachment { name: name.into(), media_type: "image/png".into(), data: base64::engine::general_purpose::STANDARD.encode(bytes), element: None };
        let names = save_attachments(home.path(), "t1", &[a("../../etc/shot.png", b"one"), a("shot.png", b"two")]).unwrap();
        assert_eq!(names, vec!["shot.png", "1-shot.png"], "a name can't climb out of the task's folder, and two don't clash");
        let mut t = task("t1", TaskState::Ready, &[], 0);
        t.attachments = names;
        let back = load_attachments(home.path(), &t);
        assert_eq!(back.iter().map(|x| (x.name.as_str(), x.media_type.as_str())).collect::<Vec<_>>(), vec![("shot.png", "image/png"), ("1-shot.png", "image/png")]);
        assert_eq!(base64::engine::general_purpose::STANDARD.decode(&back[1].data).unwrap(), b"two");
    }

    #[test]
    fn the_task_prompt_carries_what_you_asked_for_since() {
        let mut t = task("a", TaskState::Ready, &[], 0);
        t.spec = "Use the existing theme tokens.".into();
        t.notes.push(tessel_proto::task::TaskNote { at_ms: 1, by: "you".into(), text: "keep the old shortcut".into() });
        t.notes.push(tessel_proto::task::TaskNote { at_ms: 2, by: "tessel".into(), text: "proved: failed".into() });
        let p = task_prompt(&t);
        assert!(p.starts_with("do a\n\nUse the existing theme tokens."));
        assert!(p.contains("- keep the old shortcut") && !p.contains("proved: failed"));
        assert!(!p.contains("tessel_prove"), "how a task works is the agent's to know, not something shown as yours");
    }
}
