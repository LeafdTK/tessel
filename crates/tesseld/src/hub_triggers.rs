use super::*;
use std::collections::BTreeMap;
use crate::triggers::{Fired, Local, Watch};
use tessel_proto::{Pick, Trigger, Workflow};

#[derive(Default)]
pub struct State {
    pub fired: Option<Fired>,
    pub workflows: Vec<(Workflow, Option<String>)>,
    pub loaded_ms: u64,
    pub minute: String,
    pub watched_ms: u64,
    pub polled_ms: u64,
    pub watches: HashMap<String, Watch>,
    pub scanning: HashSet<String>,
    pub last: HashMap<String, (u64, String)>,
}

fn trigger_key(w: &Workflow, i: usize) -> String {
    format!("{}#{i}", w.path)
}

impl Hub {
    fn fired_path(&self) -> std::path::PathBuf {
        self.cfg.paths.home.join("triggers.json")
    }

    fn load_trigger_workflows(&mut self) {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        let mut places: Vec<Option<String>> = vec![None];
        let mut repos: Vec<String> = self.settings.projects.clone().unwrap_or_default();
        repos.extend(self.sessions.values().map(|l| l.state.meta.worktree.as_ref().map_or_else(|| l.state.meta.cwd.clone(), |w| w.repo.clone())));
        repos.sort();
        repos.dedup();
        places.extend(repos.into_iter().map(Some));
        for cwd in places {
            for w in crate::workflow::load_all(&self.cfg.paths.home, cwd.as_deref().map(Path::new)) {
                if !w.on.is_empty() && w.problems.is_empty() && seen.insert(w.path.clone()) {
                    out.push((w, cwd.clone()));
                }
            }
        }
        self.triggers.workflows = out;
    }

    pub(super) fn triggers_tick(&mut self) {
        let now = now_ms();
        if self.triggers.fired.is_none() {
            self.triggers.fired = Some(Fired::load(&self.fired_path()));
        }
        if now.saturating_sub(self.triggers.loaded_ms) >= 30_000 || self.triggers.loaded_ms == 0 {
            self.triggers.loaded_ms = now;
            self.load_trigger_workflows();
        }
        let stamp = Local::at((now / 1000) as i64).stamp();
        if stamp != self.triggers.minute {
            self.triggers.minute = stamp.clone();
            let local = Local::at((now / 1000) as i64);
            let due: Vec<(Workflow, Option<String>, String)> = self
                .triggers
                .workflows
                .iter()
                .flat_map(|(w, cwd)| {
                    let stamp = stamp.clone();
                    w.on.iter().filter_map(move |t| match t {
                        Trigger::Cron { at } if crate::triggers::Cron::parse(at).is_ok_and(|c| c.matches(&local)) => Some((w.clone(), cwd.clone(), format!("cron:{}:{at}:{stamp}", w.path))),
                        _ => None,
                    })
                })
                .collect();
            for (w, cwd, key) in due {
                let at = match w.on.iter().find(|t| matches!(t, Trigger::Cron { .. })) {
                    Some(Trigger::Cron { at }) => at.clone(),
                    _ => String::new(),
                };
                self.fire(&w, cwd, &key, serde_json::json!({"title": format!("{} at {}:{}", w.name, &stamp[8..10], &stamp[10..]), "schedule": at, "time": stamp}));
            }
        }
        if now.saturating_sub(self.triggers.watched_ms) >= self.cfg.watch_every.as_millis() as u64 {
            self.triggers.watched_ms = now;
            let watching: Vec<(String, Workflow, Option<String>, String, Vec<String>)> = self
                .triggers
                .workflows
                .iter()
                .flat_map(|(w, cwd)| {
                    w.on.iter().enumerate().filter_map(move |(i, t)| match t {
                        Trigger::Files { folder, paths } => Some((trigger_key(w, i), w.clone(), cwd.clone(), folder.clone(), paths.clone())),
                        _ => None,
                    })
                })
                .collect();
            for (key, w, cwd, folder, paths) in watching {
                if !self.triggers.scanning.insert(key.clone()) {
                    continue;
                }
                let folder = crate::projects::expand(&crate::workflow::render(&folder, &BTreeMap::new(), &HashMap::new()));
                let tx = self.tx.clone();
                let id = w.id.clone();
                tokio::task::spawn_blocking(move || {
                    let files = crate::triggers::snapshot(&folder, &paths);
                    let _ = tx.send(HubMsg::Seen { key, workflow: id, cwd, files });
                });
            }
        }
        if now.saturating_sub(self.triggers.polled_ms) >= 60_000 || self.triggers.polled_ms == 0 {
            self.triggers.polled_ms = now;
            for (w, cwd) in self.triggers.workflows.clone() {
                for (i, t) in w.on.iter().enumerate() {
                    if let Trigger::Label { repo, label } = t {
                        let (tx, repo, label, id, cwd, key) = (self.tx.clone(), repo.clone(), label.clone(), w.id.clone(), cwd.clone(), trigger_key(&w, i));
                        tokio::spawn(async move {
                            match crate::triggers::labeled(&repo, &label).await {
                                Ok(issues) => {
                                    let _ = tx.send(HubMsg::Labeled { workflow: id, cwd, trigger: key, issues });
                                }
                                Err(e) => eprintln!("tesseld: watching {repo} for {label}: {e}"),
                            }
                        });
                    }
                }
            }
        }
    }

    fn workflow_named(&self, id: &str, path_or_key: Option<&str>) -> Option<(Workflow, Option<String>)> {
        self.triggers.workflows.iter().find(|(w, _)| w.id == id && path_or_key.is_none_or(|k| k.starts_with(&w.path))).cloned()
    }

    pub(super) fn files_seen(&mut self, key: String, workflow: String, _cwd: Option<String>, files: BTreeMap<String, u128>) {
        self.triggers.scanning.remove(&key);
        let Some((w, cwd)) = self.workflow_named(&workflow, Some(&key)) else { return };
        let Some(changed) = self.triggers.watches.entry(key.clone()).or_default().step(files) else { return };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        std::hash::Hash::hash(&(now_ms() / 1000, &changed), &mut h);
        let title = match changed.as_slice() {
            [one] => format!("{one} changed"),
            many => format!("{} files changed", many.len()),
        };
        self.fire(&w, cwd, &format!("files:{key}:{:x}", std::hash::Hasher::finish(&h)), serde_json::json!({"title": title, "files": changed.join("\n")}));
    }

    pub(super) fn labeled(&mut self, workflow: String, _cwd: Option<String>, trigger: String, issues: Vec<serde_json::Value>) {
        let Some((w, cwd)) = self.workflow_named(&workflow, Some(&trigger)) else { return };
        let Some(Trigger::Label { repo, .. }) = trigger.rsplit_once('#').and_then(|(_, i)| i.parse::<usize>().ok()).and_then(|i| w.on.get(i)).cloned() else { return };
        let fired = self.triggers.fired.get_or_insert_with(Default::default);
        let first_look = fired.first(&format!("baseline:{trigger}"), now_ms());
        for issue in issues {
            let n = issue.get("number").and_then(serde_json::Value::as_i64).unwrap_or(0);
            let key = format!("label:{trigger}:{n}");
            if first_look {
                self.triggers.fired.get_or_insert_with(Default::default).first(&key, now_ms());
                continue;
            }
            self.fire(&w, cwd.clone(), &key, crate::triggers::issue_event(&repo, &issue));
        }
        if let Some(f) = &self.triggers.fired {
            f.save(&self.fired_path());
        }
    }

    fn fire(&mut self, w: &Workflow, cwd: Option<String>, key: &str, event: serde_json::Value) {
        if self.settings.paused {
            return;
        }
        let path = self.fired_path();
        let fired = self.triggers.fired.get_or_insert_with(Default::default);
        if !fired.first(key, now_ms()) {
            return;
        }
        fired.save(&path);
        let busy = self.runs.values().any(|r| r.workflow.path == w.path && !r.run.done());
        let said = if busy {
            "skipped: the last run is still going".to_owned()
        } else {
            match self.run_triggered(w, cwd, event) {
                Ok(_) => "ran".to_owned(),
                Err(e) => format!("couldn't run: {}", e.message),
            }
        };
        self.triggers.last.insert(w.path.clone(), (now_ms(), said));
    }

    fn run_triggered(&mut self, w: &Workflow, cwd: Option<String>, event: serde_json::Value) -> Result<Response, RpcError> {
        let mut picked: BTreeMap<String, Pick> = BTreeMap::new();
        for i in &w.inputs {
            if let Some(d) = &i.default {
                picked.insert(i.id.clone(), Pick { value: d.clone(), label: crate::workflow::plain(d) });
            }
        }
        let label = event.get("title").and_then(serde_json::Value::as_str).unwrap_or("").to_owned();
        picked.insert("event".into(), Pick { value: event, label });
        self.run_workflow(w.id.clone(), picked, cwd)
    }

    pub(super) fn fire_by_hand(&mut self, workflow: &str, event: serde_json::Value, cwd: Option<String>) -> Result<Response, RpcError> {
        let w = crate::workflow::find(&self.cfg.paths.home, cwd.as_deref().map(Path::new), workflow).ok_or_else(|| RpcError::new(ErrorCode::NotFound, format!("there's no workflow {workflow}")))?;
        let event = if event.is_null() { serde_json::json!({"title": "by hand"}) } else { event };
        let r = self.run_triggered(&w, cwd, event)?;
        self.triggers.last.insert(w.path.clone(), (now_ms(), "ran by hand".into()));
        Ok(r)
    }

    pub(super) fn triggers_response(&mut self) -> Response {
        self.load_trigger_workflows();
        let triggers = self
            .triggers
            .workflows
            .iter()
            .flat_map(|(w, _)| {
                let last = self.triggers.last.get(&w.path).cloned();
                w.on.iter().map(move |t| tessel_proto::workflow::TriggerInfo { workflow: w.id.clone(), name: w.name.clone(), trigger: t.clone(), last_ms: last.as_ref().map(|l| l.0), last: last.as_ref().map(|l| l.1.clone()) })
            })
            .collect();
        Response::Triggers { triggers }
    }
}
