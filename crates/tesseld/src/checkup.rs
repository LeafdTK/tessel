use std::path::PathBuf;
use std::sync::Arc;
use tessel_agent::Backend;
use tessel_proto::proof::{Proof, Reviewer, Severity, Verdict};

use crate::proof::{Ask, Change, Places, Room};
use crate::ship::git;

#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub title: String,
    pub spec: String,
}

pub fn failures(p: &Proof) -> Vec<Failure> {
    let mut out = Vec::new();
    for v in p.verify.iter().filter(|v| !v.ok) {
        out.push(Failure { title: format!("Fix `{}` failing on main", v.command.chars().take(60).collect::<String>()), spec: format!("The nightly checkup ran `{}` on the main branch and it failed:\n\n```\n{}\n```\n\nMake it pass again.", v.command, v.output) });
    }
    for t in p.tried.iter().filter(|t| !t.ok && !t.broken) {
        let said = if t.holdout { "A hidden scenario failed. What the tester saw:".to_owned() } else { format!("The scenario in .tessel/scenarios/{}.md failed. What the tester saw:", t.id) };
        out.push(Failure { title: format!("Fix: {} fails on main", t.title), spec: format!("{said}\n\n{}\n\nMake it work again.", t.said) });
    }
    for f in p.standing().filter(|f| f.lens == "look" && f.severity != Severity::Note) {
        out.push(Failure { title: format!("Fix how it looks: {}", f.title), spec: format!("The nightly checkup looked at {} and saw: {}\n\n{}", f.file.as_deref().unwrap_or("the app"), f.title, f.why) });
    }
    out
}

pub async fn run(backend: Arc<dyn Backend>, root: PathBuf, places: Places, room: Room, now: fn() -> u64) -> (Proof, Vec<Failure>) {
    let mut p = Proof { started_ms: now(), ..Default::default() };
    let main = crate::ship::main_branch(&root).await;
    let scratch = places.tessel_home.join("checkups").join(format!("{}-{}", root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), now()));
    let _ = std::fs::create_dir_all(scratch.parent().unwrap_or(&scratch));
    if let Err(e) = git(&root, &["worktree", "add", "-q", "--detach", &scratch.to_string_lossy(), &main]).await {
        p.verdict = Verdict::Broken;
        p.problem = Some(format!("couldn't check out {main}: {e}"));
        p.finished_ms = Some(now());
        return (p, Vec::new());
    }
    p.head = git(&scratch, &["rev-parse", "HEAD"]).await.unwrap_or_default();
    let mut cfg = crate::rules::config(&scratch, &scratch);
    cfg.scenarios = crate::rules::Scenarios::Always;
    let book = crate::rules::load_with(&scratch, &scratch, places.holdouts.as_deref());
    let change = Change { head: p.head.clone(), paths: Vec::new(), ..Default::default() };
    let ask = Ask { goal: format!("The nightly checkup of {main}: everything should work and look right, as it is now."), said: Vec::new() };
    let lab = match &cfg.lab {
        Some(name) => match crate::lab::Lab::open(&places.tessel_home, name, &scratch, &cfg.lab_setup).await {
            Ok(l) => {
                p.lab = Some(l.name.clone());
                Some(l)
            }
            Err(e) => {
                p.problem = Some(format!("the lab {name} couldn't take it, so it ran here: {e}"));
                None
            }
        },
        None => None,
    };
    for command in &cfg.verify {
        p.verify.push(match &lab {
            Some(l) => l.run(command, &[]).await,
            None => crate::proof::run_check(&scratch, command, None).await,
        });
    }
    for sc in book.scenarios.iter().filter(|s| s.run.is_some()) {
        let command = sc.run.clone().unwrap_or_default();
        let v = match &lab {
            Some(l) => l.run(&command, &[]).await,
            None => crate::proof::run_check(&scratch, &command, None).await,
        };
        p.tried.push(crate::proof::scripted(sc, v));
    }
    let mut shots = Vec::new();
    if !cfg.look.is_empty()
        && let Some(dir) = places.shots.clone()
    {
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        for command in &cfg.look {
            let v = match &lab {
                Some(l) => l.run(&format!("mkdir -p {} && {command}", crate::lab::SHOTS), &[("TESSEL_SHOTS", &l.shots_dir())]).await,
                None => crate::proof::run_with(&scratch, command, None, &[("TESSEL_SHOTS", &dir.to_string_lossy())]).await,
            };
            if !v.ok {
                p.verify.push(v);
            }
        }
        if let Some(l) = &lab {
            l.fetch_shots(&dir).await;
        }
        shots = crate::proof::collect_shots(&dir);
        p.shots = shots.iter().map(|s| s.to_string_lossy().into_owned()).collect();
    }
    if let Some(l) = &lab {
        l.close().await;
    }
    let tight = room.used >= 0.95 && !room.anyway;
    if tight {
        p.skipped = true;
    } else {
        let model = if room.used >= 0.8 { "haiku".to_owned() } else if matches!(cfg.model.as_str(), "auto" | "haiku") { "sonnet".to_owned() } else { cfg.model.clone() };
        if !shots.is_empty() {
            let rules: Vec<&tessel_proto::proof::Rule> = book.rules.iter().filter(|r| r.check == tessel_proto::proof::Check::Look).collect();
            let mut job = tessel_agent::backend::Job::review(crate::proof::look_prompt(&ask, &change, &shots, &rules), scratch.clone(), &model);
            job.tools = "Read".into();
            job.dirs = places.shots.iter().cloned().collect();
            let names: Vec<String> = shots.iter().filter_map(|s| s.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
            let start = std::time::Instant::now();
            match backend.job(job).await {
                Ok(r) => {
                    p.cost_usd += r.cost_usd;
                    let found = crate::proof::parse_findings(&["look".into()], &r.text, &names, &rules.iter().map(|r| r.id.clone()).collect::<Vec<_>>());
                    p.reviewers.push(Reviewer { lens: "look".into(), error: found.as_ref().err().cloned(), cost_usd: r.cost_usd, ms: start.elapsed().as_millis() as u64 });
                    p.findings.extend(found.unwrap_or_default());
                }
                Err(e) => p.reviewers.push(Reviewer { lens: "look".into(), error: Some(format!("{e:#}")), cost_usd: 0.0, ms: start.elapsed().as_millis() as u64 }),
            }
        }
        for sc in book.scenarios.iter().filter(|s| s.run.is_none()).take(cfg.testers.max(1) * 2) {
            let t = crate::proof::try_scenario(&backend, &scratch, &scratch, &places, sc, &ask, &change, &model).await;
            p.cost_usd += t.cost_usd;
            p.tried.push(t);
        }
    }
    let _ = git(&root, &["worktree", "remove", "--force", &scratch.to_string_lossy()]).await;
    p.verdict = crate::proof::verdict(&p);
    p.finished_ms = Some(now());
    let f = failures(&p);
    (p, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::proof::{Finding, Tried, Verified};

    #[test]
    fn each_thing_that_broke_on_main_becomes_one_task_that_says_what_was_seen() {
        let p = Proof {
            verify: vec![Verified { command: "cargo test".into(), ok: false, output: "test x ... FAILED".into(), ms: 1, rule: None }, Verified { command: "cargo fmt --check".into(), ok: true, output: String::new(), ms: 1, rule: None }],
            tried: vec![
                Tried { id: "sign-in".into(), title: "Signing in".into(), ok: false, said: "the button did nothing".into(), ms: 1, cost_usd: 0.0, holdout: false, broken: false },
                Tried { id: "secret".into(), title: "Paying".into(), ok: false, said: "500 on checkout".into(), ms: 1, cost_usd: 0.0, holdout: true, broken: false },
                Tried { id: "flaky".into(), title: "Search".into(), ok: false, said: "no network".into(), ms: 1, cost_usd: 0.0, holdout: false, broken: true },
            ],
            findings: vec![Finding { lens: "look".into(), rule: None, file: Some("board.png".into()), line: None, severity: Severity::Warn, title: "Buttons touch the card edge".into(), why: "no padding".into(), confirmed: None }],
            ..Default::default()
        };
        let f = failures(&p);
        assert_eq!(f.iter().map(|f| f.title.as_str()).collect::<Vec<_>>(), vec!["Fix `cargo test` failing on main", "Fix: Signing in fails on main", "Fix: Paying fails on main", "Fix how it looks: Buttons touch the card edge"]);
        assert!(f[0].spec.contains("test x ... FAILED"));
        assert!(f[2].spec.contains("hidden scenario") && !f[2].spec.contains(".tessel/scenarios"), "a holdout's file isn't pointed at");
    }
}
