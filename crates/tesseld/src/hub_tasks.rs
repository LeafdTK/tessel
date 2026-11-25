use super::*;
use super::flow::{Fact, Retry};
use crate::board::{self, Next};
use tessel_proto::task::{FailureClass, Task, TaskAct, TaskEventKind as Kind, TaskNote, TaskOption, TaskState};

const NOBODY: ClientId = ClientId::MAX;

pub const DEFAULT_PARALLEL: u32 = 3;

pub(super) const PAUSED: &str = "paused";

fn first_problem(p: &tessel_proto::proof::Proof) -> Option<String> {
    if let Some(v) = p.verify.iter().find(|v| !v.ok) {
        return Some(format!("`{}` fails", v.command));
    }
    if let Some(t) = p.tried.iter().find(|t| !t.ok && !t.broken) {
        return Some(format!("{} doesn't work", t.title));
    }
    let blocking: Vec<&tessel_proto::proof::Finding> = p.standing().filter(|f| f.severity == tessel_proto::proof::Severity::Block).collect();
    let first = blocking.first()?;
    let at = first.file.as_ref().map(|f| format!(" in {f}")).unwrap_or_default();
    Some(match blocking.len() {
        1 => format!("{}{at}", first.title),
        n => format!("{}{at}, and {} more", first.title, n - 1),
    })
}
pub(super) const WAITING: &str = "finished while paused: it's checked when the factory resumes";
const HELD: &str = "paused before its next option";

fn lower_first(s: &str) -> String {
    let s = s.trim().trim_end_matches('.');
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_lowercase().chain(c).collect(),
        None => String::new(),
    }
}

impl Hub {
    pub(super) fn task_saved(&mut self, id: &str) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.updated_ms = now_ms();
        let t = t.clone();
        let events = self.unsaved_events.remove(id).unwrap_or_default();
        if let Ok(body) = serde_json::to_string(&t) {
            self.store.save_task(&t.id, body, events.clone());
        }
        self.tell_everyone(Event::TaskUpdated { task: t });
        for row in events {
            self.tell_everyone(Event::TaskEvent { entry: row.event });
        }
    }

    pub(super) fn note(&mut self, id: &str, by: &str, text: impl Into<String>) {
        if let Some(t) = self.tasks.get_mut(id) {
            t.notes.push(TaskNote { at_ms: now_ms(), by: by.to_owned(), text: text.into() });
        }
    }

    fn task_of(&self, session: SessionId) -> Option<String> {
        self.tasks.values().find(|t| t.session == Some(session) && !t.state.finished()).map(|t| t.id.clone())
    }

    pub(super) fn tell_agent(&mut self, session: SessionId, text: String) -> bool {
        self.send_user(session, Uuid::new_v4(), text, Vec::new()).is_ok()
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn task_create_from(&mut self, repo: String, title: String, spec: String, deps: Vec<String>, parent: Option<String>, ready: bool, budget_usd: Option<f64>, plan: bool, from: Option<SessionId>) -> Result<Response, RpcError> {
        let planner = from.and_then(|s| self.tasks.values().find(|t| t.plan && t.session == Some(s) && matches!(t.state, TaskState::Running | TaskState::Review | TaskState::Planned)).cloned());
        let Some(p) = planner else {
            return self.task_create(repo, title, spec, deps, parent, ready, budget_usd, plan);
        };
        let r = self.task_create(p.repo.clone(), title, spec, deps, Some(p.id.clone()), true, budget_usd, false)?;
        if let Response::Task { task } = &r {
            let filed = format!("its planner filed “{}” ({})", task.title, task.id);
            match p.state {
                TaskState::Review => {
                    let _ = self.advance(&p.id, TaskState::Planned, Kind::Split, filed);
                }
                _ => self.record(&p.id, Kind::Split, filed),
            }
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
            land: None,
            ask: None,
            summary: None,
            changes: Vec::new(),
            review: None,
            cleared: false,
            shots: Vec::new(),
            options: Vec::new(),
            building: None,
            source: None,
            link: None,
            failure: None,
            created_ms: now,
            updated_ms: now,
        };
        let said = if ready { "filed, ready to start" } else { "filed as a draft" };
        self.tasks.insert(id.clone(), task);
        self.state_since.insert(id.clone(), now);
        self.created(&id, said);
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
        let mut changed = Vec::new();
        if let Some(title) = title.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty() && *n != t.title) {
            t.title = title;
            changed.push("its title");
        }
        if let Some(spec) = spec.filter(|s| *s != t.spec) {
            t.spec = spec;
            changed.push("what it asks");
        }
        if let Some(deps) = deps.filter(|d| *d != t.deps) {
            t.deps = deps;
            changed.push("what it waits for");
        }
        if let Some(n) = note.filter(|n| !n.trim().is_empty()) {
            self.note(id, "agent", n);
            changed.push("a note");
        }
        match changed.as_slice() {
            [] => self.task_saved(id),
            all => self.record(id, Kind::Edited, format!("changed {}", all.join(", "))),
        }
        self.dispatch();
        Ok(Response::Task { task: self.tasks[id].clone() })
    }

    pub(super) fn task_act(&mut self, id: &str, act: TaskAct) -> Result<Response, RpcError> {
        let Some(t) = self.tasks.get(id).cloned() else { return Err(RpcError::new(ErrorCode::NotFound, format!("there's no task {id}"))) };
        let refuse = |why: &str| Err(RpcError::new(ErrorCode::Conflict, format!("{id} is {:?}: {why}", t.state).to_lowercase()));
        let moved = |r: Result<(), super::flow::Illegal>| r.map_err(|e| RpcError::new(ErrorCode::Conflict, format!("{id} can't go from {} to {}", e.from.word(), e.to.word())));
        match act {
            TaskAct::Ready => {
                if !matches!(t.state, TaskState::Draft | TaskState::Dropped) {
                    return refuse("only a draft can be made ready");
                }
                let said = if t.state == TaskState::Dropped { "started again by you" } else { "started by you" };
                moved(self.advance(id, TaskState::Ready, Kind::Ready, said))?;
            }
            TaskAct::Plan => {
                if !matches!(t.state, TaskState::Draft | TaskState::Ready) {
                    return refuse("only a task that hasn't started can be planned");
                }
                self.tasks.get_mut(id).unwrap().plan = true;
                moved(self.advance(id, TaskState::Ready, Kind::Plan, "to be planned into smaller tasks first"))?;
            }
            TaskAct::Draft => {
                if t.state != TaskState::Ready {
                    return refuse("only a ready task goes back to draft");
                }
                moved(self.advance(id, TaskState::Draft, Kind::Draft, "put back as a draft"))?;
            }
            TaskAct::Land => {
                if t.state != TaskState::Review {
                    return refuse("it lands from review");
                }
                self.stop_swatches(id);
                match t.pr.clone() {
                    Some(url) if t.notes.iter().rev().find(|n| n.by == "tessel").is_some_and(|n| n.text.starts_with("its checks passed")) => {
                        moved(self.advance(id, TaskState::Checking, Kind::Merging, "you said land: merging its pull request"))?;
                        self.note(id, "tessel", "merging its pull request");
                        self.merge_pr(id, url);
                    }
                    _ => {
                        moved(self.advance(id, TaskState::Landing, Kind::Landing, "you said land"))?;
                        self.land_queue.push_back(id.to_owned());
                    }
                }
            }
            TaskAct::Change { text } => {
                if text.trim().is_empty() {
                    return Err(RpcError::new(ErrorCode::Invalid, "say what to change"));
                }
                let live = t.session.filter(|s| self.sessions.contains_key(s) && matches!(t.state, TaskState::Review | TaskState::Running));
                let to = if live.is_some() { TaskState::Running } else { TaskState::Ready };
                if !crate::machine::legal(t.state, to) {
                    return refuse("it can't be changed now");
                }
                self.stop_swatches(id);
                self.note(id, "you", text.clone());
                moved(self.advance(id, to, Kind::Changed, format!("you asked for a change: {}", text.trim())))?;
                if let Some(s) = live {
                    self.tell_agent(s, text);
                }
            }
            TaskAct::Drop => {
                if t.state.finished() {
                    return refuse("it's finished already");
                }
                self.drop_task(id, Fact::new(Kind::Dropped, "dropped by you").failed(FailureClass::Cancelled));
            }
            TaskAct::Choose { index } => {
                if t.state != TaskState::Review || t.options.is_empty() {
                    return refuse("only a question with options is answered by choosing");
                }
                let Some(o) = t.options.get(index).cloned() else { return Err(RpcError::new(ErrorCode::Invalid, format!("{id} has no option {}", index + 1))) };
                let live = t.session.and_then(|s| self.sessions.get(&s).map(|l| (s, std::path::PathBuf::from(&l.state.meta.cwd))));
                if let Some(sha) = &o.sha {
                    let Some((s, ws)) = &live else { return refuse("its workspace is gone") };
                    if board::git(ws, &["reset", "--hard", sha]).is_none() {
                        return Err(RpcError::new(ErrorCode::Conflict, format!("couldn't put {id} back to that option")));
                    }
                    self.choose_note(id, &o);
                    self.tasks.get_mut(id).unwrap().options.clear();
                    moved(self.advance(id, TaskState::Proving, Kind::Chose, format!("you chose {}: proving it", o.label)))?;
                    let s = *s;
                    if !self.proving.contains(&s) {
                        self.start_proof(s, false, false);
                    }
                } else {
                    self.choose_note(id, &o);
                    let what = if o.what.trim().is_empty() { String::new() } else { format!(" {}", o.what.trim()) };
                    let told = live.is_some_and(|(s, _)| self.tell_agent(s, format!("The person chose: {}.{what} Make it so, commit, then stop.", o.label)));
                    self.tasks.get_mut(id).unwrap().options.clear();
                    let (to, said) = if told { (TaskState::Running, format!("you chose {}: its agent makes it so", o.label)) } else { (TaskState::Ready, format!("you chose {}: it starts again", o.label)) };
                    moved(self.advance(id, to, Kind::Chose, said))?;
                }
            }
            TaskAct::Clear => {
                if !t.state.finished() {
                    return refuse("only finished work is cleared");
                }
                self.tasks.get_mut(id).unwrap().cleared = true;
                self.record(id, Kind::Cleared, "cleared off the board");
            }
            TaskAct::Unknown => return Err(RpcError::new(ErrorCode::Invalid, "tessel doesn't know that")),
        }
        if let Some(t) = self.tasks.get_mut(id)
            && t.state != TaskState::Review
        {
            t.ask = None;
        }
        self.task_saved(id);
        self.dispatch();
        self.pump_land();
        Ok(Response::Task { task: self.tasks[id].clone() })
    }

    fn choose_note(&mut self, id: &str, o: &TaskOption) {
        let text = if o.what.trim().is_empty() { format!("Chose: {}", o.label) } else { format!("Chose: {} — {}", o.label, o.what.trim()) };
        self.note(id, "you", text);
    }

    fn drop_task(&mut self, id: &str, fact: Fact) {
        let Some(t) = self.tasks.get(id).cloned() else { return };
        self.stop_swatches(id);
        if let Some(s) = t.session
            && let Some(live) = self.sessions.get_mut(&s)
            && let Some(agent) = &live.agent
            && live.state.turn_open()
        {
            agent.interrupt();
        }
        self.land_queue.retain(|x| x != id);
        if self.landing.as_deref() == Some(id) {
            self.landing = None;
        }
        let _ = self.advance_fact(id, TaskState::Dropped, fact);
        let waiting: Vec<String> = self.tasks.values().filter(|x| x.deps.iter().any(|d| d == id) && matches!(x.state, TaskState::Ready | TaskState::Draft)).map(|x| x.id.clone()).collect();
        for w in waiting {
            let x = self.tasks.get_mut(&w).unwrap();
            x.deps.retain(|d| d != id);
            let said = format!("“{}”, which it waited for, was dropped", t.title);
            self.note(&w, "tessel", said.clone());
            let _ = self.advance(&w, TaskState::Draft, Kind::Unblocked, said);
        }
        if let Some(p) = t.parent.clone() {
            self.parent_progress(&p);
        }
    }

    fn mode(&self) -> &str {
        board::mode(&self.settings.autonomy, &self.settings.bother)
    }

    fn hands_off(&self) -> bool {
        matches!(self.mode(), "full" | "ui")
    }

    fn set_aside(&mut self, id: &str, failure: Option<FailureClass>, why: String) {
        self.note(id, "tessel", format!("set aside: {}", lower_first(&why)));
        if let Some(t) = self.tasks.get_mut(id) {
            t.ask = Some(why.clone());
        }
        self.drop_task(id, Fact::new(Kind::SetAside, format!("set aside: {}", lower_first(&why))).failed(failure));
    }

    pub(super) fn hold(&mut self, id: &str, failure: FailureClass, why: impl Into<String>) {
        let why = why.into();
        if self.hands_off() {
            self.set_aside(id, Some(failure), why);
            return;
        }
        self.stop_swatches(id);
        self.note(id, "tessel", why.clone());
        let _ = self.advance_fact(id, TaskState::Review, Fact::new(Kind::Held, why).failed(failure));
    }

    pub(super) fn dispatch(&mut self) {
        if self.settings.paused {
            return;
        }
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

    pub(super) fn start_task(&mut self, id: &str) {
        if self.tasks.get(id).is_some_and(|t| t.plan) {
            self.start_planner(id);
            return;
        }
        let again = self.tasks.get(id).is_some_and(|t| t.state == TaskState::Running);
        let said = if again { "making its workspace again" } else { "dispatched: making its workspace" };
        if self.advance(id, TaskState::Running, Kind::Dispatched, said).is_err() {
            return;
        }
        let Some(t) = self.tasks.get(id) else { return };
        let (repo, title, prompt) = (t.repo.clone(), t.title.clone(), board::task_prompt(t));
        let attachments = crate::board::load_attachments(&self.cfg.paths.home, t);
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        let create = Create {
            cmd_id: Uuid::new_v4(),
            cwd: repo,
            goal: Some(title.clone()),
            prompt: Some(prompt),
            attachments,
            model: Some(f.model.clone().unwrap_or_else(|| self.settings.role("build"))),
            parent: None,
            fork: None,
            task: Some(id.to_owned()),
        };
        let name = self.free_name(id, &title, &create.cwd);
        self.make_worktree(NOBODY, 0, create, WorktreeSpec { base: None, name: Some(name) });
    }

    fn free_name(&self, id: &str, title: &str, repo: &str) -> String {
        let base = format!("{id}-{}", crate::workflow::slug(title).chars().take(30).collect::<String>());
        let repo = std::path::Path::new(repo);
        let dir = self.cfg.paths.workspaces.join(repo.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "repo".into()));
        let taken = |n: &str| dir.join(n).exists() || board::git(repo, &["show-ref", "--verify", "--quiet", &format!("refs/heads/tessel/{n}")]).is_some();
        (1..20).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| !taken(n)).unwrap_or_else(|| format!("{base}-{}", now_ms()))
    }

    fn start_planner(&mut self, id: &str) {
        if self.advance(id, TaskState::Running, Kind::Dispatched, "dispatched to its planner").is_err() {
            return;
        }
        let Some(t) = self.tasks.get(id) else { return };
        let (repo, title, prompt) = (t.repo.clone(), t.title.clone(), board::plan_prompt(t));
        let f = board::factory(std::path::Path::new(&repo), std::path::Path::new(&repo));
        let create = Create {
            cmd_id: Uuid::new_v4(),
            cwd: repo.clone(),
            goal: Some(format!("Plan: {title}")),
            prompt: Some(prompt),
            attachments: Vec::new(),
            model: Some(f.model.clone().unwrap_or_else(|| self.settings.role("plan"))),
            parent: None,
            fork: None,
            task: Some(id.to_owned()),
        };
        let result = self.create_session(create, repo, None);
        self.task_started(id, &result);
    }

    pub(super) fn parent_progress(&mut self, parent: &str) {
        if self.tasks.get(parent).is_none_or(|p| p.state != TaskState::Planned) {
            return;
        }
        let kids: Vec<TaskState> = self.tasks.values().filter(|t| t.parent.as_deref() == Some(parent)).map(|t| t.state).collect();
        if kids.is_empty() || !kids.iter().all(|s| s.finished()) {
            return;
        }
        let landed = kids.iter().filter(|s| **s == TaskState::Landed).count();
        if landed == kids.len() {
            self.tasks.get_mut(parent).unwrap().landed = Some(format!("{landed} tasks"));
            self.note(parent, "tessel", format!("all {landed} of its tasks landed"));
            let _ = self.advance(parent, TaskState::Landed, Kind::Landed, format!("all {landed} of its tasks landed"));
        } else {
            self.hold(parent, FailureClass::Cancelled, format!("{landed} of its {} tasks landed; the rest were dropped", kids.len()));
        }
    }

    pub(super) fn task_started(&mut self, id: &str, result: &Result<Response, RpcError>) {
        match result {
            Ok(Response::Session { summary }) => {
                let Some(t) = self.tasks.get_mut(id) else { return };
                t.session = Some(summary.id);
                let state = t.state;
                if state.finished() {
                    self.interrupt_session(summary.id);
                    self.record(id, Kind::Started, format!("its workspace came after it was {}: its agent was stopped", state.word()));
                } else {
                    self.record(id, Kind::Started, format!("its agent started in {}", summary.cwd));
                }
            }
            Ok(_) => {}
            Err(e) => {
                let why = format!("couldn't make its workspace: {}", e.message);
                if self.retry_or(id, FailureClass::Infra, &why, Retry::Start) {
                    return;
                }
                if self.hands_off() {
                    self.set_aside(id, Some(FailureClass::Infra), why);
                } else {
                    self.note(id, "tessel", why.clone());
                    let _ = self.advance_fact(id, TaskState::Draft, Fact::new(Kind::Held, why).failed(FailureClass::Infra));
                }
            }
        }
        self.task_saved(id);
    }

    pub(super) fn task_turn_ended(&mut self, session: SessionId) -> bool {
        let Some(id) = self.task_of(session) else { return false };
        if self.tasks[&id].state != TaskState::Running {
            return true;
        }
        if self.redo_turn(&id, session) {
            return true;
        }
        if self.tasks[&id].plan {
            let filed = self.tasks.values().filter(|t| t.parent.as_deref() == Some(id.as_str())).count();
            if filed == 0 {
                self.hold(&id, FailureClass::NoDiff, "the planner filed no tasks");
            } else {
                self.note(&id, "tessel", format!("split into {filed} tasks"));
                let _ = self.advance(&id, TaskState::Planned, Kind::Split, format!("split into {filed} tasks"));
            }
            self.task_saved(&id);
            self.dispatch();
            return true;
        }
        if self.settings.paused {
            let interrupted = self.tasks[&id].notes.last().is_some_and(|n| n.by == "tessel" && n.text == PAUSED);
            if !interrupted {
                self.note(&id, "tessel", WAITING);
                self.record(&id, Kind::Paused, "its agent finished while the factory was paused: it's proved when it resumes");
            }
            return true;
        }
        let errored = self.errored(session);
        if self.tasks[&id].building.is_some() {
            self.swatch_turned(&id, session, errored);
            self.task_saved(&id);
            self.dispatch();
            return true;
        }
        if errored {
            self.errored_turn(&id, session);
            self.task_saved(&id);
            self.dispatch();
            return true;
        }
        let said = self.sessions.get(&session).and_then(|l| {
            l.state.items().iter().rev().find_map(|i| match &i.body {
                tessel_proto::ItemBody::Text { text, .. } if !text.trim().is_empty() => Some(text.trim().chars().take(1500).collect::<String>()),
                _ => None,
            })
        });
        let t = self.tasks.get_mut(&id).unwrap();
        if said.is_some() {
            t.summary = said;
        }
        let cost = self.sessions.get(&session).map_or(0.0, |l| l.state.cost_usd());
        if self.advance_fact(&id, TaskState::Proving, Fact::new(Kind::TurnEnded, "its agent finished a turn: proving it").cost(cost)).is_ok() && !self.proving.contains(&session) {
            self.start_proof(session, false, false);
        }
        true
    }

    fn errored(&self, session: SessionId) -> bool {
        self.sessions.get(&session).and_then(|l| l.state.attention.as_ref()).is_some_and(|a| a.reason == tessel_proto::AttentionReason::Errored)
    }

    pub(super) fn factory_paused(&mut self, paused: bool) {
        let running: Vec<(String, Option<SessionId>)> = self.tasks.values().filter(|t| t.state == TaskState::Running).map(|t| (t.id.clone(), t.session)).collect();
        if paused {
            for (id, session) in running {
                let open = session.and_then(|s| self.sessions.get(&s)).is_some_and(|l| l.agent.is_some() && l.state.turn_open());
                if open {
                    if let Some(agent) = session.and_then(|s| self.sessions.get(&s)).and_then(|l| l.agent.as_ref()) {
                        agent.interrupt();
                    }
                    self.note(&id, "tessel", PAUSED);
                    self.record(&id, Kind::Paused, "the factory paused: its agent was interrupted");
                }
            }
            return;
        }
        for (id, session) in running {
            let Some(s) = session else { continue };
            let last = self.tasks[&id].notes.last().filter(|n| n.by == "tessel").map(|n| n.text.clone());
            if self.sessions.get(&s).is_some_and(|l| l.state.turn_open()) || self.shooting.contains(&id) {
                continue;
            }
            if self.tasks[&id].building.is_some() && matches!(last.as_deref(), Some(WAITING) | Some(HELD)) {
                if last.as_deref() == Some(WAITING) {
                    let errored = self.errored(s);
                    self.swatch_turned(&id, s, errored);
                } else {
                    self.swatch_go(&id);
                }
                self.task_saved(&id);
                continue;
            }
            match last.as_deref() {
                Some(WAITING) => {}
                _ if self.sessions.get(&s).is_some_and(|l| l.agent.is_none()) || last.as_deref() == Some(PAUSED) => {
                    self.note(&id, "tessel", "resumed");
                    if self.tell_agent(s, "The factory was paused while you worked. Carry on with the task where you left off.".into()) {
                        self.record(&id, Kind::Resumed, "the factory resumed: its agent carries on");
                    } else {
                        self.hold(&id, FailureClass::Infra, "its agent couldn't carry on after the pause");
                    }
                }
                _ => {}
            }
            if last.as_deref() == Some(WAITING) && self.advance(&id, TaskState::Proving, Kind::Resumed, "the factory resumed: proving what its agent finished").is_ok() && !self.proving.contains(&s) {
                self.start_proof(s, false, false);
            }
            self.task_saved(&id);
        }
        self.recheck_waiting();
        self.dispatch();
        self.pump_land();
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
            t.changes = proof.changes.clone();
            t.shots = proof.shots.clone();
        }
        let word = crate::proof::verdict_word(proof);
        let problem = first_problem(proof).map(|p| format!(": {p}")).unwrap_or_default();
        self.record_fact(&id, Fact::new(Kind::Proof, format!("proved: {word}{problem}")).cost(proof.cost_usd).extra(serde_json::json!({"verdict": proof.verdict})));
        match next {
            Next::Fix(text) => {
                self.tasks.get_mut(&id).unwrap().attempts += 1;
                let said = format!("{word}: sent back to fix ({} of {})", t.attempts + 1, f.fix_rounds);
                self.note(&id, "tessel", said.clone());
                if self.tell_agent(session, text.clone()) {
                    let _ = self.advance(&id, TaskState::Running, Kind::SentBack, said);
                } else if !self.retry_or(&id, FailureClass::Infra, "couldn't send it back to its agent", Retry::Tell { session, text, at: TaskState::Proving, to: TaskState::Running }) {
                    self.hold(&id, FailureClass::Infra, "couldn't send it back to its agent");
                }
            }
            Next::Review(why) => {
                let (failure, why) = match why {
                    Some(w) if proof.verdict == tessel_proto::proof::Verdict::Failed => (Some(FailureClass::VerifyFailed), format!("It still fails its checks after {} tries: {}", t.attempts, first_problem(proof).unwrap_or(w))),
                    Some(w) => (Some(FailureClass::EnvBlocked), format!("It couldn't be checked: {w}")),
                    None => (None, word),
                };
                self.escalate(&id, failure, why);
            }
            Next::Land => self.final_review(&id, session, proof),
        }
        self.task_saved(&id);
        self.dispatch();
        self.pump_land();
    }

    pub(super) fn escalate(&mut self, id: &str, failure: Option<FailureClass>, why: String) {
        if self.hands_off() {
            self.set_aside(id, failure, why);
            return;
        }
        self.bring(id, Fact::new(Kind::Escalated, why).failed(failure));
    }

    fn bring(&mut self, id: &str, fact: Fact) {
        self.stop_swatches(id);
        let why = fact.text().to_owned();
        if let Some(t) = self.tasks.get_mut(id) {
            t.ask = Some(why.clone());
        }
        self.note(id, "tessel", why);
        let _ = self.advance_fact(id, TaskState::Review, fact);
    }

    pub(super) fn final_review(&mut self, id: &str, session: SessionId, proof: &tessel_proto::proof::Proof) {
        if self.mode() == "me" {
            self.escalate(id, None, "Done and checked. You review finished work yourself (Factory settings).".into());
            return;
        }
        let Some(t) = self.tasks.get(id).cloned() else { return };
        let Some(ws) = self.sessions.get(&session).map(|l| std::path::PathBuf::from(&l.state.meta.cwd)) else {
            self.escalate(id, Some(FailureClass::EnvBlocked), "Its workspace is gone.".into());
            return;
        };
        self.note(id, "tessel", "in final review");
        self.record(id, Kind::Reviewing, "in final review");
        let (tx, backend, model, report, task, autonomy, bother) = (self.tx.clone(), self.backend.clone(), self.settings.role("review"), proof.report(), id.to_owned(), self.settings.autonomy.clone(), self.settings.bother.clone());
        tokio::spawn(async move {
            let diff = crate::proof::change(&ws).await.map(|c| c.diff).unwrap_or_default();
            let mut job = tessel_agent::backend::Job::review(board::final_review_prompt(&t, &report, &diff, &autonomy, &bother), ws, &model);
            job.system = "You decide whether finished work lands, for a person who trusts you to. Be decisive and brief.".into();
            let result = backend.job(job).await.map(|r| (board::read_decision(&r.text), r.cost_usd)).map_err(|e| format!("{e:#}"));
            let _ = tx.send(HubMsg::Reviewed { task, result });
        });
    }

    pub(super) fn task_reviewed(&mut self, id: String, result: Result<(board::Decision, f64), String>) {
        let Some(t) = self.tasks.get(&id).cloned() else { return };
        if t.state != TaskState::Proving {
            return;
        }
        let f = board::factory(std::path::Path::new(&t.repo), std::path::Path::new(&t.repo));
        let risky = t.risk == Some(tessel_proto::task::Risk::High);
        match result {
            Err(e) => {
                let why = format!("The final review couldn't finish: {e}");
                let retried = t.session.is_some_and(|s| self.retry_or(&id, FailureClass::ProviderTransient, &why, Retry::Review(s)));
                if !retried {
                    self.escalate(&id, Some(FailureClass::ProviderTransient), why);
                }
            }
            Ok((decision, cost)) => {
                self.tasks.get_mut(&id).unwrap().proof_cost_usd += cost;
                let said = match &decision {
                    board::Decision::Land(why) => format!("the reviewer would land it: {why}"),
                    board::Decision::Change(why, _) => format!("the reviewer wants a change: {why}"),
                    board::Decision::Ask { why, .. } => format!("the reviewer has a question: {why}"),
                };
                self.record_fact(&id, Fact::new(Kind::Review, said).cost(cost));
                match decision {
                    board::Decision::Land(why) if risky && matches!(self.mode(), "me" | "agent") => {
                        self.tasks.get_mut(&id).unwrap().review = Some(why.clone());
                        self.escalate(&id, None, format!("It touches risky code (auth, secrets, migrations or infra), so it's yours to land. The reviewer says: {why}"));
                    }
                    board::Decision::Land(why) => self.approve(&id, why),
                    board::Decision::Ask { why, visual, .. } if self.mode() == "full" || (self.mode() == "ui" && !visual) => self.approve(&id, why),
                    board::Decision::Change(why, change) if t.attempts < f.fix_rounds && t.session.is_some_and(|s| self.sessions.contains_key(&s)) => {
                        let session = t.session.unwrap();
                        let task = self.tasks.get_mut(&id).unwrap();
                        task.attempts += 1;
                        task.review = Some(why.clone());
                        self.note(&id, "tessel", format!("sent back by the reviewer: {why}"));
                        if self.tell_agent(session, format!("The final review sent this back: {why}\n\nChange: {change}\n\nMake the change, commit, then stop: it's checked again.")) {
                            let _ = self.advance(&id, TaskState::Running, Kind::SentBack, format!("sent back by the reviewer: {why}"));
                        } else {
                            self.escalate(&id, Some(FailureClass::Infra), why);
                        }
                    }
                    board::Decision::Change(why, _) => self.escalate(&id, Some(FailureClass::JudgeVeto), format!("The reviewer still wants changes after {} tries: {why}", t.attempts)),
                    board::Decision::Ask { why, options, visual } => self.ask_with(&id, why, options, visual),
                }
            }
        }
        self.task_saved(&id);
        self.dispatch();
        self.pump_land();
    }

    fn approve(&mut self, id: &str, why: String) {
        self.tasks.get_mut(id).unwrap().review = Some(why.clone());
        self.note(id, "tessel", format!("approved: {why}"));
        if self.advance(id, TaskState::Landing, Kind::Approved, format!("approved: {why}")).is_ok() {
            self.land_queue.push_back(id.to_owned());
        }
    }

    fn task_place(&self, id: &str) -> Option<(SessionId, std::path::PathBuf, std::path::PathBuf)> {
        let s = self.tasks.get(id)?.session?;
        let live = self.sessions.get(&s)?;
        let ws = std::path::PathBuf::from(&live.state.meta.cwd);
        let root = live.state.meta.worktree.as_ref().map_or_else(|| ws.clone(), |w| std::path::PathBuf::from(&w.repo));
        Some((s, ws, root))
    }

    fn ask_with(&mut self, id: &str, why: String, options: Vec<(String, String)>, visual: bool) {
        let options: Vec<TaskOption> = options.into_iter().map(|(label, what)| TaskOption { label, what, ..Default::default() }).collect();
        let offers = matches!(self.mode(), "agent" | "ui" | "custom");
        let place = self.task_place(id).filter(|(_, ws, _)| ws.is_dir());
        let base = place
            .as_ref()
            .filter(|(_, ws, root)| visual && offers && !options.is_empty() && !crate::rules::config(ws, root).look.is_empty())
            .and_then(|(_, ws, _)| board::git(ws, &["rev-parse", "HEAD"]));
        let Some(base) = base else {
            self.bring(id, Fact::new(Kind::Asked, why));
            if offers && let Some(t) = self.tasks.get_mut(id) {
                t.options = options;
            }
            return;
        };
        board::save_swatch(&self.cfg.paths.home, id, &board::Swatch { base, why });
        let n = options.len();
        let t = self.tasks.get_mut(id).unwrap();
        t.options = options;
        t.building = Some(0);
        let said = format!("the reviewer has a question: trying its {n} options so you can see them");
        self.note(id, "tessel", said.clone());
        if self.advance(id, TaskState::Running, Kind::Options, said).is_ok() {
            self.swatch_go(id);
        }
    }

    fn swatch_go(&mut self, id: &str) {
        let Some(t) = self.tasks.get(id).cloned() else { return };
        let (Some(i), Some(s)) = (t.building, t.session) else { return };
        let (Some(o), Some(sw)) = (t.options.get(i).cloned(), board::load_swatch(&self.cfg.paths.home, id)) else {
            self.swatch_done(id);
            return;
        };
        if self.settings.paused {
            if t.notes.last().is_none_or(|n| n.text != HELD) {
                self.note(id, "tessel", HELD);
            }
            return;
        }
        self.note(id, "tessel", format!("trying option {}: {}", i + 1, o.label));
        if !self.tell_agent(s, board::option_prompt(&sw.base, &o.label, &o.what)) {
            self.note(id, "tessel", "its agent couldn't try the options");
            self.swatch_done(id);
        }
    }

    fn swatch_turned(&mut self, id: &str, session: SessionId, errored: bool) {
        let Some(i) = self.tasks.get(id).and_then(|t| t.building) else { return };
        let base = board::load_swatch(&self.cfg.paths.home, id).map(|s| s.base);
        let place = self.task_place(id);
        let head = place.as_ref().and_then(|(_, ws, _)| board::git(ws, &["rev-parse", "HEAD"]));
        let failed = if errored {
            Some("the agent stopped with an error")
        } else if head.is_none() || head == base {
            Some("nothing was committed")
        } else {
            None
        };
        if let Some(why) = failed {
            self.note(id, "tessel", format!("option {} didn't work out: {why}", i + 1));
            self.swatch_next(id);
            return;
        }
        let (Some((_, ws, root)), Some(sha)) = (place, head) else { return };
        if let Some(o) = self.tasks.get_mut(id).and_then(|t| t.options.get_mut(i)) {
            o.sha = Some(sha);
        }
        self.shooting.insert(id.to_owned());
        let home = self.cfg.paths.home.clone();
        let dir = home.join("shots").join(session.to_string()).join(format!("option-{i}"));
        let (tx, task) = (self.tx.clone(), id.to_owned());
        tokio::spawn(async move {
            let result = crate::proof::shoot(&home, &ws, &root, &dir).await.map(|shots| shots.iter().map(|p| p.to_string_lossy().into_owned()).collect());
            let _ = tx.send(HubMsg::Swatch { task, index: i, result });
        });
    }

    pub(super) fn swatch_shot(&mut self, id: String, index: usize, result: Result<Vec<String>, String>) {
        self.shooting.remove(&id);
        if self.tasks.get(&id).is_none_or(|t| t.building != Some(index) || t.state != TaskState::Running) {
            return;
        }
        match result {
            Ok(shots) => {
                if let Some(o) = self.tasks.get_mut(&id).and_then(|t| t.options.get_mut(index)) {
                    o.shots = shots;
                }
            }
            Err(e) => self.note(&id, "tessel", format!("option {} has no screenshots: {e}", index + 1)),
        }
        self.swatch_next(&id);
        self.task_saved(&id);
        self.dispatch();
    }

    fn swatch_next(&mut self, id: &str) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        let next = t.building.map_or(0, |i| i + 1);
        if next >= t.options.len() {
            self.swatch_done(id);
            return;
        }
        t.building = Some(next);
        self.swatch_go(id);
    }

    pub(super) fn swatch_done(&mut self, id: &str) {
        let home = self.cfg.paths.home.clone();
        let sw = board::load_swatch(&home, id);
        if let (Some(sw), Some((_, ws, _))) = (&sw, self.task_place(id)) {
            board::git(&ws, &["reset", "--hard", &sw.base]);
        }
        board::forget_swatch(&home, id);
        self.shooting.remove(id);
        if let Some(t) = self.tasks.get_mut(id) {
            t.building = None;
        }
        let why = sw.map(|s| s.why).unwrap_or_else(|| "The reviewer has a question.".into());
        let options = self.tasks.get(id).map(|t| t.options.clone()).unwrap_or_default();
        self.bring(id, Fact::new(Kind::Asked, why));
        if let Some(t) = self.tasks.get_mut(id).filter(|t| t.state == TaskState::Review) {
            t.options = options;
        }
    }

    fn stop_swatches(&mut self, id: &str) {
        let Some(t) = self.tasks.get_mut(id) else { return };
        t.options.clear();
        if t.building.take().is_none() {
            return;
        }
        let home = self.cfg.paths.home.clone();
        let quiet = !self.shooting.remove(id) && self.task_place(id).is_some_and(|(s, _, _)| self.sessions.get(&s).is_some_and(|l| !l.state.turn_open()));
        if quiet
            && let (Some(sw), Some((_, ws, _))) = (board::load_swatch(&home, id), self.task_place(id))
        {
            board::git(&ws, &["reset", "--hard", &sw.base]);
        }
        board::forget_swatch(&home, id);
    }

    pub(super) fn pump_land(&mut self) {
        if self.landing.is_some() || self.settings.paused {
            return;
        }
        while let Some(id) = self.land_queue.pop_front() {
            let Some(t) = self.tasks.get(&id).filter(|t| t.state == TaskState::Landing).cloned() else { continue };
            let Some(ws) = t.session.and_then(|s| self.sessions.get(&s)).map(|l| l.state.meta.cwd.clone()) else {
                self.hold(&id, FailureClass::EnvBlocked, "its workspace is gone, so there's nothing to land");
                continue;
            };
            let root = std::path::PathBuf::from(&t.repo);
            let (pr, issue) = (t.pr.clone(), t.issue.clone());
            let verify = crate::rules::config(std::path::Path::new(&ws), &root).verify;
            let mode = t.land.as_deref().map(board::parse_land).or(board::factory(std::path::Path::new(&ws), &root).land).unwrap_or_else(|| board::parse_land(&self.settings.land));
            self.landing = Some(id.clone());
            let since = self.relanding.remove(&id).then(|| self.state_since.get(&id).copied().unwrap_or(t.updated_ms));
            let (tx, backend) = (self.tx.clone(), self.backend.clone());
            tokio::spawn(async move {
                let ws = std::path::Path::new(&ws);
                let already = match since {
                    Some(since) => crate::land::already(ws, &root, &mode, since).await,
                    None => None,
                };
                let result = match already {
                    Some(done) => Ok(done),
                    None => crate::land::land(backend, ws, &root, &t.title, &verify, mode, pr.as_deref(), issue.as_deref()).await,
                };
                let _ = tx.send(HubMsg::Landed { task: id, result });
            });
            return;
        }
    }

    pub(super) fn task_landed(&mut self, id: String, result: Result<crate::land::Landed, crate::land::Stuck>) {
        use crate::land::{Landed, Stuck};
        if self.landing.as_deref() == Some(id.as_str()) {
            self.landing = None;
        }
        let Some(state) = self.tasks.get(&id).map(|t| t.state) else { return };
        match (&result, state) {
            (_, TaskState::Landing) => {}
            (Ok(_), TaskState::Review) => {
                let _ = self.advance(&id, TaskState::Landing, Kind::Recovered, "its landing finished after all");
            }
            (late, state) => {
                let what = match late {
                    Ok(Landed::Merged { sha, .. }) => Some(format!("landed {sha} after it was {}", state.word())),
                    Ok(Landed::Kept { branch, .. }) => Some(format!("kept on {branch} after it was {}", state.word())),
                    Ok(Landed::Opened { url, .. }) => Some(format!("opened {url} after it was {}", state.word())),
                    Err(_) => None,
                };
                if let Some(what) = what {
                    self.record(&id, Kind::Landed, what);
                }
                self.dispatch();
                self.pump_land();
                return;
            }
        }
        let parent = self.tasks.get(&id).and_then(|t| t.parent.clone());
        let session = self.tasks.get(&id).and_then(|t| t.session);
        match result {
            Ok(Landed::Merged { sha, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.link = crate::ship::web_url(&t.repo).map(|u| format!("{u}/commit/{sha}"));
                t.landed = Some(sha.clone());
                self.note(&id, "tessel", said.clone());
                let _ = self.advance_fact(&id, TaskState::Landed, Fact::new(Kind::Landed, format!("{said} ({sha})")).extra(serde_json::json!({"sha": sha})));
                self.grade_landed(&id);
            }
            Ok(Landed::Kept { branch, said }) => {
                let t = self.tasks.get_mut(&id).unwrap();
                t.link = crate::ship::web_url(&t.repo).map(|u| format!("{u}/tree/{branch}"));
                t.landed = Some(branch.clone());
                self.note(&id, "tessel", said.clone());
                let _ = self.advance_fact(&id, TaskState::Landed, Fact::new(Kind::Landed, said).extra(serde_json::json!({"branch": branch})));
            }
            Ok(Landed::Opened { url, said }) => {
                self.tasks.get_mut(&id).unwrap().pr = Some(url.clone());
                self.ci_polled.remove(&id);
                self.note(&id, "tessel", format!("{said} Waiting for its checks."));
                let _ = self.advance_fact(&id, TaskState::Checking, Fact::new(Kind::OpenedPr, format!("{said} {url}")).extra(serde_json::json!({"pr": url})));
            }
            Err(stuck) => {
                let (class, said, ask) = match stuck {
                    Stuck::Conflict(files) => (
                        FailureClass::LandConflict,
                        format!("conflicts with main in {}", files.join(", ")),
                        Some(format!(
                            "Landing this hit conflicts with the main branch in: {}. Rebase onto main, resolve them keeping what both sides meant, run the checks, commit, then stop.",
                            files.join(", ")
                        )),
                    ),
                    Stuck::Checks(out) => (FailureClass::VerifyFailed, "checks failed on top of main".to_owned(), Some(format!("On top of the main branch the checks fail:\n\n{out}\n\nRebase onto main, fix it, commit, then stop."))),
                    Stuck::Nothing => (FailureClass::NoDiff, "there's nothing to land".to_owned(), None),
                    Stuck::Blocked(e) => (FailureClass::EnvBlocked, e, None),
                    Stuck::Other(e) => (FailureClass::Infra, e, None),
                };
                let sent = match (ask, session) {
                    (Some(text), Some(s)) => self.tell_agent(s, text),
                    _ => false,
                };
                if sent {
                    self.note(&id, "tessel", format!("couldn't land: {said}"));
                    let _ = self.advance_fact(&id, TaskState::Running, Fact::new(Kind::SentBack, format!("couldn't land: {said}: sent back to fix")).failed(class));
                } else if !self.retry_or(&id, class, &format!("couldn't land: {said}"), Retry::Land) {
                    self.hold(&id, class, format!("couldn't land: {said}"));
                }
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
        if self.settings.paused {
            return;
        }
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
                    self.record_fact(&id, Fact::new(Kind::Ci, format!("couldn't ask GitHub about its pull request: {e}")).failed(FailureClass::Infra));
                }
            }
            Ok(Ci::Pending) => return,
            Ok(Ci::Merged(sha)) => {
                let task = self.tasks.get_mut(&id).unwrap();
                let landed = if sha.is_empty() { task.pr.clone().unwrap_or_default() } else { sha };
                task.landed = Some(landed.clone());
                self.note(&id, "tessel", "Merged its pull request.");
                let _ = self.advance_fact(&id, TaskState::Landed, Fact::new(Kind::Merged, format!("merged its pull request ({landed})")).extra(serde_json::json!({"sha": landed})));
                self.grade_landed(&id);
                if let Some(p) = t.parent.clone() {
                    self.parent_progress(&p);
                }
            }
            Ok(Ci::Closed) => self.hold(&id, FailureClass::Cancelled, "its pull request was closed"),
            Ok(Ci::Passed) => {
                let f = board::factory(std::path::Path::new(&t.repo), std::path::Path::new(&t.repo));
                if (f.auto_land && t.risk == Some(tessel_proto::task::Risk::Low)) || self.hands_off() {
                    self.note(&id, "tessel", "its checks passed: merging");
                    self.record(&id, Kind::Merging, "its checks passed: merging");
                    self.merge_pr(&id, t.pr.clone().unwrap_or_default());
                } else {
                    self.note(&id, "tessel", "its checks passed: Land merges it");
                    let _ = self.advance(&id, TaskState::Review, Kind::Ci, "its checks passed: Land merges it");
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
                    self.tasks.get_mut(&id).unwrap().attempts += 1;
                    let said = format!("its checks failed ({}): sent back to fix", names.join(", "));
                    self.note(&id, "tessel", said.clone());
                    let _ = self.advance_fact(&id, TaskState::Running, Fact::new(Kind::Ci, said).failed(FailureClass::VerifyFailed));
                } else {
                    self.hold(&id, FailureClass::VerifyFailed, format!("its checks failed on GitHub: {}", names.join(", ")));
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
            check_model: self.settings.role("check"),
            look_model: self.settings.role("look"),
            test_model: self.settings.role("test"),
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
        }
        if ready && self.tasks.get(id).is_some_and(|t| t.state != TaskState::Ready) {
            let _ = self.advance(id, TaskState::Ready, Kind::Ready, "ready to start");
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
        }
        self.note(&id, "tessel", format!("from #{}", issue.number));
        self.record_fact(&id, Fact::new(Kind::Filed, format!("filed from issue #{}", issue.number)).extra(serde_json::json!({"issue": issue.url})));
        if ready {
            let _ = self.advance(&id, TaskState::Ready, Kind::Ready, "ready to start");
        }
        self.dispatch();
        Ok(Response::Task { task: self.tasks[&id].clone() })
    }

    fn poll_issues(&mut self) {
        if self.settings.paused {
            return;
        }
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
        self.factory_tick();
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
            self.hold(&id, FailureClass::Timeout, "stopped: it reached its budget");
        }
        if self.tasks.values().any(|t| t.state == TaskState::Ready) {
            self.dispatch();
        }
    }

    pub(super) fn recheck_waiting(&mut self) {
        if self.settings.paused {
            return;
        }
        let old: Vec<(String, SessionId)> = self
            .tasks
            .values()
            .filter(|t| t.state == TaskState::Review && t.ask.is_none())
            .filter_map(|t| Some((t.id.clone(), t.session?)))
            .filter(|(_, s)| self.sessions.get(s).is_some_and(|l| std::path::Path::new(&l.state.meta.cwd).is_dir()))
            .collect();
        for (id, s) in old {
            self.note(&id, "tessel", "checking it again for the final review");
            if self.advance(&id, TaskState::Proving, Kind::Resumed, "checking it again for the final review").is_ok() && !self.proving.contains(&s) {
                self.start_proof(s, false, false);
            }
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
