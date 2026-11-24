use super::*;
use crate::board::{self, Next};
use tessel_proto::task::{Task, TaskAct, TaskNote, TaskState};

const NOBODY: ClientId = ClientId::MAX;

pub const DEFAULT_PARALLEL: u32 = 3;

impl Hub {
    fn task_saved(&mut self, id: &str) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.updated_ms = now_ms();
        let t = t.clone();
        if let Ok(body) = serde_json::to_string(&t) {
            self.store.save_task(&t.id, body);
        }
        self.tell_everyone(Event::TaskUpdated { task: t });
    }

    fn note(&mut self, id: &str, by: &str, text: impl Into<String>) {
        if let Some(t) = self.tasks.get_mut(id) {
            t.notes.push(TaskNote { at_ms: now_ms(), by: by.to_owned(), text: text.into() });
        }
    }

    fn task_of(&self, session: SessionId) -> Option<String> {
        self.tasks.values().find(|t| t.session == Some(session) && !t.state.finished()).map(|t| t.id.clone())
    }

    fn tell_agent(&mut self, session: SessionId, text: String) -> bool {
        self.send_user(session, Uuid::new_v4(), text, Vec::new()).is_ok()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_create_from(&mut self, repo: String, title: String, spec: String, deps: Vec<String>, parent: Option<String>, ready: bool, budget_usd: Option<f64>, plan: bool, from: Option<SessionId>) -> Result<Response, RpcError> {
        let planner = from.and_then(|s| self.tasks.values().find(|t| t.plan && t.session == Some(s) && matches!(t.state, TaskState::Running | TaskState::Review | TaskState::Planned)).cloned());
        let Some(p) = planner else {
            return self.task_create(repo, title, spec, deps, parent, ready, budget_usd, plan);
        };
        let r = self.task_create(p.repo.clone(), title, spec, deps, Some(p.id.clone()), true, budget_usd, false)?;
        if p.state == TaskState::Review {
            self.tasks.get_mut(&p.id).unwrap().state = TaskState::Planned;
            self.task_saved(&p.id);
        }
        Ok(r)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_create(&mut self, repo: String, title: String, spec: String, deps: Vec<String>, parent: Option<String>, ready: bool, budget_usd: Option<f64>, plan: bool) -> Result<Response, RpcError> {
        let title = title.trim().to_owned();
        if title.is_empty() {
            return Err(RpcError::new(ErrorCode::Invalid, "a task needs a title"));
        }
        let repo = crate::projects::expand(&repo);
        if !repo.is_dir() {
            return Err(RpcError::new(ErrorCode::Invalid, format!("{} isn't a folder here", repo.display())));
        }
        if let Some(missing) = deps.iter().chain(parent.iter()).find(|d| !self.tasks.contains_key(*d)) {
            return Err(RpcError::new(ErrorCode::NotFound, format!("there's no task {missing}")));
        }
        let repo = board::repo_root(&repo).to_string_lossy().into_owned();
        let id = board::new_id(|id| self.tasks.contains_key(id));
        let now = now_ms();
        let task = Task {
            id: id.clone(),
            title,
            spec,
            repo,
            state: if ready { TaskState::Ready } else { TaskState::Draft },
            deps,
            parent,
            session: None,
            notes: Vec::new(),
            plan,
            attempts: 0,
            proof_cost_usd: 0.0,
            budget_usd,
            risk: None,
            landed: None,
            pr: None,
            issue: None,
            attachments: Vec::new(),
            created_ms: now,
            updated_ms: now,
        };
        self.tasks.insert(id.clone(), task);
        self.task_saved(&id);
        self.dispatch();
        Ok(Response::Task { task: self.tasks[&id].clone() })
    }

    pub(super) fn task_edit(&mut self, id: &str, title: Option<String>, spec: Option<String>, deps: Option<Vec<String>>, note: Option<String>) -> Result<Response, RpcError> {
        if let Some(ds) = &deps
            && let Some(bad) = ds.iter().find(|d| *d == id || !self.tasks.contains_key(*d))
        {
            return Err(RpcError::new(ErrorCode::Invalid, format!("{bad} can't be one it waits on")));
        }
        let Some(t) = self.tasks.get_mut(id) else { return Err(RpcError::new(ErrorCode::NotFound, format!("there's no task {id}"))) };
        if let Some(title) = title.map(|t| t.trim().to_owned()).filter(|t| !t.is_empty()) {
            t.title = title;
        }
        if let Some(spec) = spec {
            t.spec = spec;
        }
        if let Some(deps) = deps {
            t.deps = deps;
        }
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            self.note(id, "agent", n);
        }
        self.task_saved(id);
        self.dispatch();
        Ok(Response::Task { task: self.tasks[id].clone() })
    }

    pub(super) fn task_act(&mut self, id: &str, act: TaskAct) -> Result<Response, RpcError> {
        let Some(t) = self.tasks.get(id).cloned() else { return Err(RpcError::new(ErrorCode::NotFound, format!("there's no task {id}"))) };
        let refuse = |why: &str| Err(RpcError::new(ErrorCode::Conflict, format!("{id} is {:?}: {why}", t.state).to_lowercase()));
        match act {
            TaskAct::Ready => {
                if !matches!(t.state, TaskState::Draft | TaskState::Dropped) {
                    return refuse("only a draft can be made ready");
                }
                self.tasks.get_mut(id).unwrap().state = TaskState::Ready;
            }
            TaskAct::Plan => {
                if !matches!(t.state, TaskState::Draft | TaskState::Ready) {
                    return refuse("only a task that hasn't started can be planned");
                }
                let task = self.tasks.get_mut(id).unwrap();
                task.plan = true;
                task.state = TaskState::Ready;
            }
            TaskAct::Draft => {
                if t.state != TaskState::Ready {
                    return refuse("only a ready task goes back to draft");
                }
                self.tasks.get_mut(id).unwrap().state = TaskState::Draft;
            }
            TaskAct::Land => {
                if t.state != TaskState::Review {
                    return refuse("it lands from review");
                }
                match t.pr.clone() {
                    Some(url) if t.notes.iter().rev().find(|n| n.by == "tessel").is_some_and(|n| n.text.starts_with("its checks passed")) => {
                        self.tasks.get_mut(id).unwrap().state = TaskState::Checking;
                        self.note(id, "tessel", "merging its pull request");
                        self.merge_pr(id, url);
                    }
                    _ => {
                        self.tasks.get_mut(id).unwrap().state = TaskState::Landing;
                        self.land_queue.push_back(id.to_owned());
                    }
                }
            }
            TaskAct::Change { text } => {
                if text.trim().is_empty() {
                    return Err(RpcError::new(ErrorCode::Invalid, "say what to change"));
                }
                self.note(id, "you", text.clone());
                match t.session.filter(|s| self.sessions.contains_key(s) && matches!(t.state, TaskState::Review | TaskState::Running)) {
                    Some(s) => {
                        self.tasks.get_mut(id).unwrap().state = TaskState::Running;
                        self.tell_agent(s, text);
                    }
                    None => self.tasks.get_mut(id).unwrap().state = TaskState::Ready,
                }
            }
            TaskAct::Drop => {
                if let Some(s) = t.session
                    && let Some(live) = self.sessions.get_mut(&s)
                    && let Some(agent) = &live.agent
                    && live.state.turn_open()
                {
                    agent.interrupt();
                }
                self.land_queue.retain(|x| x != id);
                self.tasks.get_mut(id).unwrap().state = TaskState::Dropped;
                if let Some(p) = t.parent.clone() {
                    self.task_saved(id);
                    self.parent_progress(&p);
                }
            }
            TaskAct::Unknown => return Err(RpcError::new(ErrorCode::Invalid, "tessel doesn't know that")),
        }
        self.task_saved(id);
        self.dispatch();
        self.pump_land();
        Ok(Response::Task { task: self.tasks[id].clone() })
    }

    pub(super) fn dispatch(&mut self) {
        let parallel = self.settings.parallel.unwrap_or(DEFAULT_PARALLEL) as usize;
        let mut active = self.tasks.values().filter(|t| t.state.active()).count();
        for id in board::ready(&self.tasks) {
            if active >= parallel {
                break;
            }
            self.start_task(&id);
            active += 1;
        }
    }

    fn start_task(&mut self, id: &str) {
        if self.tasks.get(id).is_some_and(|t| t.plan) {
            self.start_planner(id);
            return;
        }
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.state = TaskState::Running;
        let (repo, title, prompt) = (t.repo.clone(), t.title.clone(), board::task_prompt(t));
        let attachments = crate::board::load_attachments(&self.cfg.paths.home, t);
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        self.task_saved(id);
        let create = Create {
            cmd_id: Uuid::new_v4(),
            cwd: repo,
            goal: Some(title.clone()),
            prompt: Some(prompt),
            attachments,
            model: Some(f.model),
            parent: None,
            fork: None,
            task: Some(id.to_owned()),
        };
        let name = format!("{id}-{}", crate::workflow::slug(&title).chars().take(30).collect::<String>());
        self.make_worktree(NOBODY, 0, create, WorktreeSpec { base: None, name: Some(name) });
    }

    fn start_planner(&mut self, id: &str) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.state = TaskState::Running;
        let (repo, title, prompt) = (t.repo.clone(), t.title.clone(), board::plan_prompt(t));
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        let create = Create {
            cmd_id: Uuid::new_v4(),
            cwd: repo.clone(),
            goal: Some(format!("Plan: {title}")),
            prompt: Some(prompt),
            attachments: Vec::new(),
            model: Some(f.model),
            parent: None,
            fork: None,
            task: Some(id.to_owned()),
        };
        let result = self.create_session(create, repo, None);
        self.task_started(id, &result);
    }

    fn parent_progress(&mut self, parent: &str) {
        if self.tasks.get(parent).is_none_or(|p| p.state != TaskState::Planned) {
            return;
        }
        let kids: Vec<TaskState> = self.tasks.values().filter(|t| t.parent.as_deref() == Some(parent)).map(|t| t.state).collect();
        if kids.is_empty() || !kids.iter().all(|s| s.finished()) {
            return;
        }
        let landed = kids.iter().filter(|s| **s == TaskState::Landed).count();
        let p = self.tasks.get_mut(parent).unwrap();
        if landed == kids.len() {
            p.state = TaskState::Landed;
            p.landed = Some(format!("{landed} tasks"));
            self.note(parent, "tessel", format!("all {landed} of its tasks landed"));
        } else {
            p.state = TaskState::Review;
            self.note(parent, "tessel", format!("{landed} of its {} tasks landed; the rest were dropped", kids.len()));
        }
        self.task_saved(parent);
    }

    pub(super) fn task_started(&mut self, id: &str, result: &Result<Response, RpcError>) {
        match result {
            Ok(Response::Session { summary }) => {
                if let Some(t) = self.tasks.get_mut(id) {
                    t.session = Some(summary.id);
                }
            }
            Ok(_) => {}
            Err(e) => {
                if let Some(t) = self.tasks.get_mut(id) {
                    t.state = TaskState::Draft;
                }
                self.note(id, "tessel", format!("couldn't make its workspace: {}", e.message));
            }
        }
        self.task_saved(id);
    }

    pub(super) fn task_turn_ended(&mut self, session: SessionId) -> bool {
        let Some(id) = self.task_of(session) else { return false };
        if self.tasks[&id].state != TaskState::Running {
            return true;
        }
        if self.tasks[&id].plan {
            let filed = self.tasks.values().filter(|t| t.parent.as_deref() == Some(id.as_str())).count();
            let t = self.tasks.get_mut(&id).unwrap();
            if filed == 0 {
                t.state = TaskState::Review;
                self.note(&id, "tessel", "the planner filed no tasks");
            } else {
                t.state = TaskState::Planned;
                self.note(&id, "tessel", format!("split into {filed} tasks"));
            }
            self.task_saved(&id);
            self.dispatch();
            return true;
        }
        let errored = self.sessions.get(&session).and_then(|l| l.state.attention.as_ref()).is_some_and(|a| a.reason == tessel_proto::AttentionReason::Errored);
        if errored {
            self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
            self.note(&id, "tessel", "the agent stopped with an error");
            self.task_saved(&id);
            self.dispatch();
            return true;
        }
        self.tasks.get_mut(&id).unwrap().state = TaskState::Proving;
        self.task_saved(&id);
        if !self.proving.contains(&session) {
            self.start_proof(session, false, false);
        }
        true
    }

    pub(super) fn task_proved(&mut self, session: SessionId, proof: &tessel_proto::proof::Proof) {
        let Some(id) = self.task_of(session) else { return };
        let t = self.tasks[&id].clone();
        if t.state != TaskState::Proving {
            return;
        }
        let f = board::factory(std::path::Path::new(&t.repo), std::path::Path::new(&t.repo));
        let next = board::after_proof(&t, proof, &f);
        {
            let t = self.tasks.get_mut(&id).unwrap();
            t.proof_cost_usd += proof.cost_usd;
            t.risk = Some(board::risk(proof));
        }
        let word = crate::proof::verdict_word(proof);
        match next {
            Next::Fix(text) => {
                self.tasks.get_mut(&id).unwrap().attempts += 1;
                self.note(&id, "tessel", format!("{word}: sent back to fix ({} of {})", t.attempts + 1, f.fix_rounds));
                if self.tell_agent(session, text) {
                    self.tasks.get_mut(&id).unwrap().state = TaskState::Running;
                } else {
                    self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                }
            }
            Next::Review(why) => {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                self.note(&id, "tessel", match why {
                    Some(w) => format!("{word}: {w}"),
                    None => word,
                });
            }
            Next::Land => {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Landing;
                self.note(&id, "tessel", format!("{word}: low risk, landing it"));
                self.land_queue.push_back(id.clone());
            }
        }
        self.task_saved(&id);
        self.dispatch();
        self.pump_land();
    }

    pub(super) fn pump_land(&mut self) {
        if self.landing.is_some() {
            return;
        }
        while let Some(id) = self.land_queue.pop_front() {
            let Some(t) = self.tasks.get(&id).filter(|t| t.state == TaskState::Landing).cloned() else { continue };
            let Some(ws) = t.session.and_then(|s| self.sessions.get(&s)).map(|l| l.state.meta.cwd.clone()) else {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                self.note(&id, "tessel", "its workspace is gone, so there's nothing to land");
                self.task_saved(&id);
                continue;
            };
            let root = std::path::PathBuf::from(&t.repo);
            let (pr, issue) = (t.pr.clone(), t.issue.clone());
            let verify = crate::rules::config(std::path::Path::new(&ws), &root).verify;
            let mode = board::factory(std::path::Path::new(&ws), &root).land;
            self.landing = Some(id.clone());
            let (tx, backend) = (self.tx.clone(), self.backend.clone());
            tokio::spawn(async move {
                let result = crate::land::land(backend, std::path::Path::new(&ws), &root, &t.title, &verify, mode, pr.as_deref(), issue.as_deref()).await;
                let _ = tx.send(HubMsg::Landed { task: id, result });
            });
            return;
        }
    }

    pub(super) fn task_landed(&mut self, id: String, result: Result<crate::land::Landed, crate::land::Stuck>) {
        use crate::land::{Landed, Stuck};
        self.landing = None;
        let parent = self.tasks.get(&id).and_then(|t| t.parent.clone());
        let session = self.tasks.get(&id).and_then(|t| t.session);
        match result {
            Ok(Landed::Merged { sha, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.state = TaskState::Landed;
                t.landed = Some(sha);
                self.note(&id, "tessel", said);
                self.grade_landed(&id);
            }
            Ok(Landed::Opened { url, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.state = TaskState::Checking;
                t.pr = Some(url);
                self.ci_polled.remove(&id);
                self.note(&id, "tessel", format!("{said} Waiting for its checks."));
            }
            Err(stuck) => {
                let (said, ask) = match stuck {
                    Stuck::Conflict(files) => (
                        format!("conflicts with main in {}", files.join(", ")),
                        Some(format!(
                            "Landing this hit conflicts with the main branch in: {}. Rebase onto main, resolve them keeping what both sides meant, run the checks, commit, then stop.",
                            files.join(", ")
                        )),
                    ),
                    Stuck::Checks(out) => ("checks failed on top of main".to_owned(), Some(format!("On top of the main branch the checks fail:\n\n{out}\n\nRebase onto main, fix it, commit, then stop."))),
                    Stuck::Other(e) => (e, None),
                };
                self.note(&id, "tessel", format!("couldn't land: {said}"));
                let sent = match (ask, session) {
                    (Some(text), Some(s)) => self.tell_agent(s, text),
                    _ => false,
                };
                self.tasks.get_mut(&id).unwrap().state = if sent { TaskState::Running } else { TaskState::Review };
            }
        }
        self.task_saved(&id);
        if let Some(p) = parent {
            self.parent_progress(&p);
        }
        self.dispatch();
        self.pump_land();
    }

    fn grade_landed(&mut self, id: &str) {
        let Some(t) = self.tasks.get(id) else { return };
        let Some(proof) = t.session.and_then(|s| self.sessions.get(&s)).and_then(|l| l.state.proof.clone()) else { return };
        let mut book = crate::grades::load(&self.cfg.paths.home);
        crate::grades::landed(&mut book, &t.repo, &proof);
        crate::grades::save(&self.cfg.paths.home, &book);
    }

    fn merge_pr(&mut self, id: &str, url: String) {
        let Some(t) = self.tasks.get(id) else { return };
        self.ci_busy.insert(id.to_owned());
        let (tx, repo, task, issue) = (self.tx.clone(), std::path::PathBuf::from(&t.repo), id.to_owned(), t.issue.clone());
        tokio::spawn(async move {
            let closes = match &issue {
                Some(u) => crate::issues::closes(&repo, u).await,
                None => None,
            };
            let result = crate::ci::merge(&repo, &url, closes).await;
            let _ = tx.send(HubMsg::Ci { task, result, why: String::new() });
        });
    }

    fn poll_ci(&mut self) {
        let due: Vec<(String, String, String)> = self
            .tasks
            .values()
            .filter(|t| t.state == TaskState::Checking && !self.ci_busy.contains(&t.id))
            .filter(|t| self.ci_polled.get(&t.id).is_none_or(|at| at.elapsed() >= std::time::Duration::from_secs(60)))
            .filter_map(|t| Some((t.id.clone(), t.repo.clone(), t.pr.clone()?)))
            .collect();
        for (id, repo, url) in due {
            self.ci_polled.insert(id.clone(), std::time::Instant::now());
            self.ci_busy.insert(id.clone());
            let tx = self.tx.clone();
            tokio::spawn(async move {
                let repo = std::path::PathBuf::from(repo);
                let result = crate::ci::poll(&repo, &url).await;
                let why = match &result {
                    Ok(crate::ci::Ci::Failed(failed)) => crate::ci::why(&repo, failed).await,
                    _ => String::new(),
                };
                let _ = tx.send(HubMsg::Ci { task: id, result, why });
            });
        }
    }

    pub(super) fn task_ci(&mut self, id: String, result: Result<crate::ci::Ci, String>, why: String) {
        use crate::ci::Ci;
        self.ci_busy.remove(&id);
        let Some(t) = self.tasks.get(&id).cloned() else { return };
        if t.state != TaskState::Checking {
            return;
        }
        match result {
            Err(e) => {
                if t.notes.last().is_none_or(|n| !n.text.starts_with("couldn't ask GitHub")) {
                    self.note(&id, "tessel", format!("couldn't ask GitHub about its pull request: {e}"));
                }
            }
            Ok(Ci::Pending) => return,
            Ok(Ci::Merged(sha)) => {
                let task = self.tasks.get_mut(&id).unwrap();
                task.state = TaskState::Landed;
                task.landed = Some(if sha.is_empty() { task.pr.clone().unwrap_or_default() } else { sha });
                self.note(&id, "tessel", "Merged its pull request.");
                self.grade_landed(&id);
                if let Some(p) = t.parent.clone() {
                    self.task_saved(&id);
                    self.parent_progress(&p);
                }
            }
            Ok(Ci::Closed) => {
                self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                self.note(&id, "tessel", "its pull request was closed");
            }
            Ok(Ci::Passed) => {
                let f = board::factory(std::path::Path::new(&t.repo), std::path::Path::new(&t.repo));
                if f.auto_land && t.risk == Some(tessel_proto::task::Risk::Low) {
                    self.note(&id, "tessel", "its checks passed: merging");
                    self.merge_pr(&id, t.pr.clone().unwrap_or_default());
                } else {
                    self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                    self.note(&id, "tessel", "its checks passed: Land merges it");
                }
            }
            Ok(Ci::Failed(failed)) => {
                let f = board::factory(std::path::Path::new(&t.repo), std::path::Path::new(&t.repo));
                let names: Vec<String> = failed.iter().map(|(n, _)| n.clone()).collect();
                let sent = t.attempts < f.fix_rounds
                    && t.session.is_some_and(|s| {
                        self.tell_agent(
                            s,
                            format!("The pull request's checks failed on GitHub:\n\n{why}\n\nFix it, commit, then stop: it's checked and pushed again."),
                        )
                    });
                if sent {
                    let t = self.tasks.get_mut(&id).unwrap();
                    t.attempts += 1;
                    t.state = TaskState::Running;
                    self.note(&id, "tessel", format!("its checks failed ({}): sent back to fix", names.join(", ")));
                } else {
                    self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
                    self.note(&id, "tessel", format!("its checks failed on GitHub: {}", names.join(", ")));
                }
            }
        }
        self.task_saved(&id);
        self.dispatch();
    }

    pub(super) fn checkup(&mut self, repo: &str, reply: Option<(ClientId, u64)>) -> Result<(), RpcError> {
        let path = std::path::Path::new(repo);
        if !path.is_dir() {
            return Err(RpcError::new(ErrorCode::Invalid, format!("{repo} isn't a folder here")));
        }
        let root = board::repo_root(path);
        let key = root.to_string_lossy().into_owned();
        if !self.checkups.insert(key.clone()) {
            return Err(RpcError::new(ErrorCode::Conflict, "a checkup of it is running already"));
        }
        let places = crate::proof::Places {
            holdouts: Some(crate::rules::holdouts(&self.cfg.paths.home, &root)),
            shots: Some(self.cfg.paths.home.join("shots").join(format!("checkup-{}", root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()))),
            home: std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()),
            tessel_home: self.cfg.paths.home.clone(),
            no_comments: self.settings.no_comments,
        };
        let room = crate::proof::room(&self.usage, (now_ms() / 1000) as i64, false);
        let (tx, backend) = (self.tx.clone(), self.backend.clone());
        tokio::spawn(async move {
            let (proof, failures) = crate::checkup::run(backend, root, places, room, now_ms).await;
            let _ = tx.send(HubMsg::CheckedUp { repo: key, proof: Box::new(proof), failures, reply });
        });
        Ok(())
    }

    pub(super) fn checked_up(&mut self, repo: String, proof: tessel_proto::proof::Proof, failures: Vec<crate::checkup::Failure>, reply: Option<(ClientId, u64)>) {
        self.checkups.remove(&repo);
        let _ = std::fs::create_dir_all(self.cfg.paths.home.join("checkups"));
        let name = std::path::Path::new(&repo).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        if let Ok(b) = serde_json::to_vec_pretty(&proof) {
            let _ = std::fs::write(self.cfg.paths.home.join("checkups").join(format!("{name}.json")), b);
        }
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        let mut filed = Vec::new();
        for fail in failures {
            let open = self.tasks.values().any(|t| t.repo == repo && !t.state.finished() && t.title == fail.title);
            if open || filed.len() >= 5 {
                continue;
            }
            if let Ok(Response::Task { task }) = self.task_create(repo.clone(), fail.title, fail.spec, Vec::new(), None, f.checkup_ready, None, false) {
                filed.push(task.id);
            }
        }
        if let Some((client, id)) = reply {
            self.respond(client, id, Ok(Response::Checkup { proof, filed }));
        }
    }

    pub(super) fn task_attach(&mut self, id: &str, attachments: Vec<tessel_proto::Attachment>, ready: bool) -> Result<Response, RpcError> {
        let names = crate::board::save_attachments(&self.cfg.paths.home, id, &attachments).map_err(|e| RpcError::new(ErrorCode::Internal, format!("couldn't keep the attachments: {e}")))?;
        if let Some(t) = self.tasks.get_mut(id) {
            t.attachments = names;
            if ready {
                t.state = TaskState::Ready;
            }
        }
        self.task_saved(id);
        self.dispatch();
        Ok(Response::Task { task: self.tasks[id].clone() })
    }

    pub(super) fn issue_fetched(&mut self, repo: String, result: Result<crate::issues::Issue, String>, ready: bool, reply: Option<(ClientId, u64)>) {
        let answer = match result {
            Ok(issue) => self.task_from(repo, issue, ready),
            Err(e) => Err(RpcError::new(ErrorCode::Invalid, e)),
        };
        if let Some((client, id)) = reply {
            self.respond(client, id, answer);
        }
    }

    pub(super) fn task_from(&mut self, repo: String, issue: crate::issues::Issue, ready: bool) -> Result<Response, RpcError> {
        if let Some(t) = self.tasks.values().find(|t| t.issue.as_deref() == Some(issue.url.as_str()) && t.state != TaskState::Dropped) {
            return Ok(Response::Task { task: t.clone() });
        }
        let spec = crate::issues::spec(&issue);
        let Ok(Response::Task { task }) = self.task_create(repo, issue.title.clone(), spec, Vec::new(), None, false, None, false) else {
            return Err(RpcError::new(ErrorCode::Invalid, format!("couldn't file #{}", issue.number)));
        };
        let id = task.id;
        if let Some(t) = self.tasks.get_mut(&id) {
            t.issue = Some(issue.url.clone());
            if ready {
                t.state = TaskState::Ready;
            }
        }
        self.note(&id, "tessel", format!("from #{}", issue.number));
        self.task_saved(&id);
        self.dispatch();
        Ok(Response::Task { task: self.tasks[&id].clone() })
    }

    fn poll_issues(&mut self) {
        if self.issues_polled.is_some_and(|at| at.elapsed() < std::time::Duration::from_secs(60)) {
            return;
        }
        self.issues_polled = Some(std::time::Instant::now());
        let mut roots: Vec<String> = self.tasks.values().map(|t| t.repo.clone()).collect();
        roots.extend(self.sessions.values().filter_map(|l| l.state.meta.worktree.as_ref().map(|w| w.repo.clone())));
        roots.sort();
        roots.dedup();
        for repo in roots {
            let root = std::path::PathBuf::from(&repo);
            let Some(label) = board::factory(&root, &root).issues else { continue };
            let known: Vec<String> = self.tasks.values().filter_map(|t| t.issue.clone()).collect();
            let tx = self.tx.clone();
            tokio::spawn(async move {
                if let Ok(found) = crate::issues::labeled(&root, &label).await {
                    let issues: Vec<crate::issues::Issue> = found.into_iter().filter(|i| !known.contains(&i.url)).collect();
                    if !issues.is_empty() {
                        let _ = tx.send(HubMsg::Issues { repo, issues });
                    }
                }
            });
        }
    }

    pub(super) fn tasks_tick(&mut self) {
        self.poll_ci();
        self.poll_issues();
        let over: Vec<(String, SessionId)> = self
            .tasks
            .values()
            .filter(|t| t.state == TaskState::Running)
            .filter_map(|t| {
                let s = t.session?;
                let budget = t.budget_usd?;
                let spent = self.sessions.get(&s).map_or(0.0, |l| l.state.cost_usd()) + t.proof_cost_usd;
                (spent >= budget).then(|| (t.id.clone(), s))
            })
            .collect();
        for (id, s) in over {
            if let Some(live) = self.sessions.get_mut(&s)
                && let Some(agent) = &live.agent
            {
                agent.interrupt();
            }
            self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
            self.note(&id, "tessel", "stopped: it reached its budget");
            self.task_saved(&id);
        }
        if self.tasks.values().any(|t| t.state == TaskState::Ready) {
            self.dispatch();
        }
    }

    pub(super) fn tasks_after_restart(&mut self) {
        let stale: Vec<String> = self.tasks.values().filter(|t| matches!(t.state, TaskState::Proving | TaskState::Landing)).map(|t| t.id.clone()).collect();
        for id in stale {
            let session = self.tasks[&id].session;
            if self.tasks[&id].state == TaskState::Proving
                && let Some(s) = session.filter(|s| self.sessions.contains_key(s))
            {
                self.start_proof(s, false, false);
                continue;
            }
            self.tasks.get_mut(&id).unwrap().state = TaskState::Review;
            self.note(&id, "tessel", "tessel restarted while landing it: land it again");
            self.task_saved(&id);
        }
    }
}

impl Hub {
    pub(super) fn key_secrets(&self) -> Vec<(tessel_proto::keys::KeyInfo, String)> {
        let home = &self.cfg.paths.home;
        crate::keys::load(home).into_iter().map(|k| {
            let s = crate::keys::secret(home, &k.name).unwrap_or_default();
            (k, s)
        }).collect()
    }

    pub(super) fn keys_response(&self) -> Response {
        let uses = self.egress.as_ref().map(|e| e.uses()).unwrap_or_default();
        let keys = self
            .key_secrets()
            .into_iter()
            .map(|(k, s)| {
                let n = uses.iter().filter(|u| u.allowed && u.key.as_deref() == Some(k.name.as_str())).count() as u32;
                tessel_proto::keys::KeyInfo { stored: !s.is_empty(), uses: n, ..k }
            })
            .collect();
        let recent = uses.into_iter().rev().take(100).collect();
        Response::Keys { keys, uses: recent, certificate: self.egress.as_ref().map(|e| e.certificate().to_string_lossy().into_owned()).unwrap_or_default() }
    }

    pub(super) fn key_set(&mut self, key: tessel_proto::keys::KeyInfo, value: Option<String>) -> Result<Response, RpcError> {
        crate::keys::check(&key).map_err(|e| RpcError::new(ErrorCode::Invalid, e))?;
        let home = self.cfg.paths.home.clone();
        if let Some(v) = value.filter(|v| !v.trim().is_empty()) {
            crate::keys::store(&home, &key.name, v.trim()).map_err(|e| RpcError::new(ErrorCode::Internal, e))?;
        }
        let mut all = crate::keys::load(&home);
        all.retain(|k| k.name != key.name);
        all.push(key);
        crate::keys::save(&home, &all).map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))?;
        if let Some(e) = &self.egress {
            e.set_keys(self.key_secrets());
        }
        Ok(self.keys_response())
    }

    pub(super) fn key_remove(&mut self, name: &str) -> Result<Response, RpcError> {
        let home = self.cfg.paths.home.clone();
        let mut all = crate::keys::load(&home);
        let before = all.len();
        all.retain(|k| k.name != name);
        if all.len() == before {
            return Err(RpcError::new(ErrorCode::NotFound, format!("there's no key {name}")));
        }
        crate::keys::forget(&home, name);
        crate::keys::save(&home, &all).map_err(|e| RpcError::new(ErrorCode::Internal, e.to_string()))?;
        if let Some(e) = &self.egress {
            e.set_keys(self.key_secrets());
        }
        Ok(self.keys_response())
    }

    pub(super) fn egress_for(&self, session: SessionId, cwd: &str, repo: &str) -> (Vec<(String, String)>, Option<String>) {
        let Some(eg) = &self.egress else { return (Vec::new(), None) };
        let sb = crate::sandbox::config(std::path::Path::new(cwd), std::path::Path::new(repo));
        let keys = crate::keys::load(&self.cfg.paths.home);
        let brokered = keys.iter().any(|k| crate::keys::secret(&self.cfg.paths.home, &k.name).is_some());
        if !brokered && !sb.sealed {
            let placeholders = keys.iter().filter_map(|k| k.env.clone().map(|e| (e, k.placeholder()))).collect();
            return (placeholders, None);
        }
        let token = eg.scope(crate::egress::Scope { session: Some(session), sealed: sb.sealed, allow: sb.allow });
        let env = eg.env(&token, sb.sealed, &keys);
        let profile = (sb.sealed && cfg!(target_os = "macos")).then(|| {
            let home = std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default());
            crate::sandbox::profile(std::path::Path::new(cwd), std::path::Path::new(repo), &home, &self.cfg.paths.home)
        });
        (env, profile)
    }
}
