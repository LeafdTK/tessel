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
    pub(super) fn task_create(&mut self, repo: String, title: String, spec: String, deps: Vec<String>, parent: Option<String>, ready: bool, budget_usd: Option<f64>) -> Result<Response, RpcError> {
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
            attempts: 0,
            proof_cost_usd: 0.0,
            budget_usd,
            risk: None,
            landed: None,
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
                self.tasks.get_mut(id).unwrap().state = TaskState::Landing;
                self.land_queue.push_back(id.to_owned());
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
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.state = TaskState::Running;
        let (repo, title, prompt) = (t.repo.clone(), t.title.clone(), board::task_prompt(t));
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        self.task_saved(id);
        let create = Create {
            cmd_id: Uuid::new_v4(),
            cwd: repo,
            goal: Some(title.clone()),
            prompt: Some(prompt),
            attachments: Vec::new(),
            model: Some(f.model),
            parent: None,
            fork: None,
            task: Some(id.to_owned()),
        };
        let name = format!("{id}-{}", crate::workflow::slug(&title).chars().take(30).collect::<String>());
        self.make_worktree(NOBODY, 0, create, WorktreeSpec { base: None, name: Some(name) });
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
            let verify = crate::rules::config(std::path::Path::new(&ws), &root).verify;
            let mode = board::factory(std::path::Path::new(&ws), &root).land;
            self.landing = Some(id.clone());
            let (tx, backend) = (self.tx.clone(), self.backend.clone());
            tokio::spawn(async move {
                let result = crate::land::land(backend, std::path::Path::new(&ws), &root, &t.title, &verify, mode).await;
                let _ = tx.send(HubMsg::Landed { task: id, result });
            });
            return;
        }
    }

    pub(super) fn task_landed(&mut self, id: String, result: Result<crate::land::Landed, crate::land::Stuck>) {
        use crate::land::{Landed, Stuck};
        self.landing = None;
        let session = self.tasks.get(&id).and_then(|t| t.session);
        match result {
            Ok(Landed::Merged { sha, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.state = TaskState::Landed;
                t.landed = Some(sha);
                self.note(&id, "tessel", said);
            }
            Ok(Landed::Opened { url, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.state = TaskState::Landed;
                t.landed = Some(url);
                self.note(&id, "tessel", said);
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
        self.dispatch();
        self.pump_land();
    }

    pub(super) fn tasks_tick(&mut self) {
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
