use serde_json::Value;
use std::path::{Path, PathBuf};
use tessel_proto::views::View;

pub fn dir(home: &Path) -> PathBuf {
    home.join("views")
}

fn binds(node: &Value, out: &mut Vec<String>) {
    if let Some(b) = node.get("p").and_then(|p| p.get("bind")).and_then(Value::as_str) {
        out.push(b.to_owned());
    }
    for c in node.get("c").and_then(Value::as_array).into_iter().flatten() {
        binds(c, out);
    }
}

pub fn check(node: &Value) -> Vec<String> {
    let mut problems = tessel_proto::surface::validate(node);
    let mut found = Vec::new();
    binds(node, &mut found);
    for b in found.into_iter().filter(|b| !tessel_proto::surface::source_known(b)) {
        problems.push(format!("\"{b}\" isn't a source: see the format for the ones there are"));
    }
    problems
}

pub fn load(home: &Path) -> Vec<View> {
    let Ok(entries) = std::fs::read_dir(dir(home)) else { return Vec::new() };
    let mut files: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let id = f.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
            let path = f.to_string_lossy().into_owned();
            match std::fs::read_to_string(&f).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()) {
                Some(v) => {
                    let node = v.get("node").cloned().unwrap_or(Value::Null);
                    let problems = if node.is_null() { vec!["it has no \"node\"".to_owned()] } else { check(&node) };
                    View { title: v.get("title").and_then(Value::as_str).map_or_else(|| id.replace(['-', '_'], " "), str::to_owned), icon: v.get("icon").and_then(Value::as_str).map(str::to_owned), id, node, path, problems }
                }
                None => View { title: id.clone(), id, path, problems: vec!["it isn't valid JSON".into()], ..Default::default() },
            }
        })
        .collect()
}

pub fn metrics(tasks: &[tessel_proto::task::Task], cost_of: impl Fn(&tessel_proto::task::Task) -> f64, reverted: &[String]) -> tessel_proto::views::Metrics {
    use tessel_proto::task::TaskState;
    let count = |s: &[TaskState]| tasks.iter().filter(|t| s.contains(&t.state)).count() as u32;
    let landed: Vec<&tessel_proto::task::Task> = tasks.iter().filter(|t| t.state == TaskState::Landed).collect();
    let cost_landed: f64 = landed.iter().map(|t| cost_of(t)).sum();
    tessel_proto::views::Metrics {
        landed: landed.len() as u32,
        review: count(&[TaskState::Review]),
        working: count(&[TaskState::Running, TaskState::Proving, TaskState::Landing]),
        ready: count(&[TaskState::Ready]),
        dropped: count(&[TaskState::Dropped]),
        cost_per_landed: if landed.is_empty() { 0.0 } else { cost_landed / landed.len() as f64 },
        waste: tasks.iter().filter(|t| t.state == TaskState::Dropped).map(&cost_of).sum(),
        escaped: landed.iter().filter(|t| t.landed.as_ref().is_some_and(|sha| reverted.iter().any(|r| r.starts_with(sha.as_str()) || sha.starts_with(r.as_str())))).count() as u32,
    }
}

pub async fn reverted(repo: &Path) -> Vec<String> {
    let main = crate::ship::main_branch(repo).await;
    let log = crate::ship::git(repo, &["log", "--format=%B", "-n", "500", &main]).await.unwrap_or_default();
    log.lines().filter_map(|l| l.trim().strip_prefix("This reverts commit ")).map(|s| s.trim_end_matches('.').to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tessel_proto::task::{Task, TaskState};

    #[test]
    fn a_view_is_checked_like_a_surface_and_its_sources_must_exist() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(super::dir(dir.path())).unwrap();
        std::fs::write(super::dir(dir.path()).join("good.json"), json!({"title": "Mine", "node": {"k": "column", "c": [{"k": "metric", "p": {"label": "Landed", "bind": "factory.landed"}}, {"k": "table", "p": {"bind": "tasks.review"}}]}}).to_string()).unwrap();
        std::fs::write(super::dir(dir.path()).join("bad.json"), json!({"node": {"k": "chart", "p": {"bind": "weather"}}}).to_string()).unwrap();
        std::fs::write(super::dir(dir.path()).join("junk.json"), "{nope").unwrap();
        let views = load(dir.path());
        let get = |id: &str| views.iter().find(|v| v.id == id).unwrap();
        assert!(get("good").problems.is_empty(), "{:?}", get("good").problems);
        assert_eq!(get("good").title, "Mine");
        assert!(get("bad").problems.iter().any(|p| p.contains("\"chart\" isn't an element")));
        assert!(get("junk").problems[0].contains("JSON"));
        assert!(check(&json!({"k": "metric", "p": {"bind": "weather"}})).iter().any(|p| p.contains("\"weather\" isn't a source")));
    }

    fn task(id: &str, state: TaskState, landed: Option<&str>) -> Task {
        Task { id: id.into(), title: id.into(), spec: String::new(), repo: "/r".into(), state, deps: vec![], parent: None, session: None, notes: vec![], plan: false, attempts: 0, proof_cost_usd: 0.1, budget_usd: None, risk: None, landed: landed.map(str::to_owned), created_ms: 0, updated_ms: 0 }
    }

    #[tokio::test]
    async fn a_reverted_commit_is_found_in_the_main_branch_history() {
        let dir = tempfile::tempdir().unwrap();
        let sh = |args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(dir.path()).args(args).output().unwrap();
            assert!(o.status.success(), "{args:?} {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_owned()
        };
        for a in [&["init", "-q", "-b", "main"][..], &["config", "user.name", "t"], &["config", "user.email", "t@example.invalid"]] {
            sh(a);
        }
        std::fs::write(dir.path().join("a.txt"), "1").unwrap();
        sh(&["add", "-A"]);
        sh(&["commit", "-q", "-m", "feat: one"]);
        std::fs::write(dir.path().join("a.txt"), "2").unwrap();
        sh(&["commit", "-q", "-am", "fix: two"]);
        let landed = sh(&["rev-parse", "--short", "HEAD"]);
        sh(&["revert", "--no-edit", "HEAD"]);
        let found = reverted(dir.path()).await;
        assert!(found.iter().any(|r| r.starts_with(&landed)), "{found:?} vs {landed}");
    }

    #[test]
    fn the_numbers_that_matter_are_cost_per_landed_change_waste_and_what_escaped() {
        let tasks = vec![task("a", TaskState::Landed, Some("abc1234")), task("b", TaskState::Landed, Some("def5678")), task("c", TaskState::Dropped, None), task("d", TaskState::Review, None), task("e", TaskState::Running, None)];
        let m = metrics(&tasks, |t| if t.id == "a" { 0.3 } else { 0.1 }, &["abc1234ffffffff".to_owned()]);
        assert_eq!((m.landed, m.review, m.working, m.dropped, m.escaped), (2, 1, 1, 1, 1));
        assert!((m.cost_per_landed - 0.2).abs() < 1e-9);
        assert!((m.waste - 0.1).abs() < 1e-9);
    }
}
