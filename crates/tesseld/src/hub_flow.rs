use super::*;
use crate::machine::{self, Bark};
use crate::store::EventRow;
use tessel_proto::task::{FailureClass, TaskEvent, TaskEventKind as Kind, TaskState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Illegal {
    pub from: TaskState,
    pub to: TaskState,
}

pub(super) struct Fact {
    kind: Kind,
    detail: String,
    failure: Option<FailureClass>,
    cost: Option<f64>,
    extra: serde_json::Value,
}

impl Fact {
    pub(super) fn new(kind: Kind, detail: impl Into<String>) -> Self {
        Self { kind, detail: detail.into(), failure: None, cost: None, extra: serde_json::Value::Null }
    }

    pub(super) fn failed(mut self, class: impl Into<Option<FailureClass>>) -> Self {
        self.failure = class.into();
        self
    }

    pub(super) fn cost(mut self, usd: f64) -> Self {
        self.cost = (usd > 0.0).then_some(usd);
        self
    }

    pub(super) fn extra(mut self, extra: serde_json::Value) -> Self {
        self.extra = extra;
        self
    }

    pub(super) fn text(&self) -> &str {
        &self.detail
    }
}

pub(super) enum Retry {
    Tell { session: SessionId, text: String, at: TaskState, to: TaskState },
    Review(SessionId),
    Land,
    Start,
}

#[derive(Default)]
pub(super) struct Retrying {
    tries: u32,
    due: Option<(std::time::Instant, Retry)>,
}

pub(super) const CARRY_ON: &str = "tessel restarted while you worked. Carry on with the task where you left off; when it's done, commit, then stop.";
const START_OVER: &str = "You went quiet for a long time, so tessel stopped you. Pick the task up again from where the work stands: look at what's there, finish it, commit, then stop.";
const NUDGE: &str = "Nothing has happened for a while. If something is in your way, say what; otherwise carry on with the task, then commit and stop.";
const AFTER_ERROR: &str = "That turn stopped with an error. Carry on with the task where you left off; when it's done, commit, then stop.";

impl Hub {
    fn log(&mut self, id: &str, from: Option<TaskState>, fact: Fact) {
        let Some(to) = self.tasks.get(id).map(|t| t.state) else { return };
        let seq = self.event_seqs.entry(id.to_owned()).or_insert(0);
        *seq += 1;
        self.next_event += 1;
        let event = TaskEvent { id: self.next_event, task_id: id.to_owned(), seq: *seq, at_ms: now_ms(), kind: fact.kind, from, to, detail: fact.detail, cost_usd: fact.cost, failure: fact.failure };
        self.unsaved_events.entry(id.to_owned()).or_default().push(EventRow { event, build: self.cfg.build.clone(), extra: fact.extra });
    }

    pub(super) fn created(&mut self, id: &str, detail: impl Into<String>) {
        let extra = self.tasks.get(id).map(|t| serde_json::json!({"title": t.title, "repo": t.repo})).unwrap_or_default();
        self.log(id, None, Fact::new(Kind::Created, detail).extra(extra));
        self.task_saved(id);
    }

    pub(super) fn record(&mut self, id: &str, kind: Kind, detail: impl Into<String>) {
        self.record_fact(id, Fact::new(kind, detail));
    }

    pub(super) fn record_fact(&mut self, id: &str, fact: Fact) {
        let Some(from) = self.tasks.get(id).map(|t| t.state) else { return };
        self.log(id, Some(from), fact);
        self.task_saved(id);
    }

    pub(super) fn advance(&mut self, id: &str, to: TaskState, kind: Kind, detail: impl Into<String>) -> Result<(), Illegal> {
        self.advance_fact(id, to, Fact::new(kind, detail))
    }

    pub(super) fn advance_fact(&mut self, id: &str, to: TaskState, fact: Fact) -> Result<(), Illegal> {
        let Some(from) = self.tasks.get(id).map(|t| t.state) else { return Err(Illegal { from: TaskState::Unknown, to }) };
        if !machine::legal(from, to) {
            let detail = format!("refused to go from {} to {} ({}: {})", from.word(), to.word(), fact.kind.word(), fact.detail);
            eprintln!("tesseld: task {id}: {detail}");
            self.log(id, Some(from), Fact { kind: Kind::Illegal, detail, failure: None, cost: None, extra: serde_json::json!({"wanted": fact.kind.word()}) });
            self.task_saved(id);
            return Err(Illegal { from, to });
        }
        let Some(t) = self.tasks.get_mut(id) else { return Err(Illegal { from, to }) };
        t.state = to;
        if fact.failure.is_some() {
            t.failure = fact.failure;
        } else if matches!(to, TaskState::Ready | TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking | TaskState::Landed) {
            t.failure = None;
        }
        if to != from {
            self.state_since.insert(id.to_owned(), now_ms());
            self.watch.remove(id);
            if matches!(to, TaskState::Review | TaskState::Landed | TaskState::Dropped | TaskState::Draft) {
                self.retries.remove(id);
                self.redo.remove(id);
            }
            if to.finished() {
                self.relanding.remove(id);
            }
        }
        self.log(id, Some(from), fact);
        self.task_saved(id);
        Ok(())
    }

    pub(super) fn retry_or(&mut self, id: &str, class: FailureClass, why: &str, retry: Retry) -> bool {
        if !class.retryable() {
            return false;
        }
        let r = self.retries.entry(id.to_owned()).or_default();
        if r.tries >= machine::MAX_RETRIES {
            return false;
        }
        let wait = machine::backoff(r.tries, machine::jitter());
        r.tries += 1;
        let n = r.tries;
        r.due = Some((std::time::Instant::now() + wait, retry));
        self.record_fact(id, Fact::new(Kind::Retried, format!("{why}: trying again in {} ms ({n} of {})", wait.as_millis(), machine::MAX_RETRIES)).failed(class));
        true
    }

    fn retry_tick(&mut self) {
        let now = std::time::Instant::now();
        let due: Vec<String> = self.retries.iter().filter(|(_, r)| r.due.as_ref().is_some_and(|(at, _)| *at <= now)).map(|(id, _)| id.clone()).collect();
        for id in due {
            let Some((_, what)) = self.retries.get_mut(&id).and_then(|r| r.due.take()) else { continue };
            let Some(state) = self.tasks.get(&id).map(|t| t.state) else { continue };
            match what {
                Retry::Tell { session, text, at, to } if state == at => {
                    if self.tell_agent(session, text.clone()) {
                        let _ = self.advance(&id, to, Kind::Retried, "tried again: its agent has it");
                    } else if !self.retry_or(&id, FailureClass::Infra, "its agent couldn't be reached", Retry::Tell { session, text, at, to }) {
                        self.hold(&id, FailureClass::Infra, format!("its agent couldn't be reached after {} tries", machine::MAX_RETRIES));
                    }
                }
                Retry::Review(session) if state == TaskState::Proving => match self.sessions.get(&session).and_then(|l| l.state.proof.clone()) {
                    Some(proof) => self.final_review(&id, session, &proof),
                    None => self.escalate(&id, Some(FailureClass::EnvBlocked), "Its proof is gone, so it can't be reviewed again.".into()),
                },
                Retry::Land if state == TaskState::Landing => {
                    self.land_queue.push_back(id.clone());
                    self.pump_land();
                }
                Retry::Start if state == TaskState::Running && self.tasks.get(&id).is_some_and(|t| t.session.is_none()) => self.start_task(&id),
                _ => {}
            }
        }
    }

    pub(super) fn interrupt_session(&mut self, s: SessionId) -> bool {
        let grace = self.cfg.interrupt_grace.as_millis() as Millis;
        let Some(live) = self.sessions.get_mut(&s) else { return false };
        let Some(agent) = &live.agent else { return false };
        if !live.state.turn_open() {
            return false;
        }
        agent.interrupt();
        live.interrupt_deadline = Some(now_ms() + grace);
        true
    }

    pub(super) fn redo_turn(&mut self, id: &str, session: SessionId) -> bool {
        if !self.redo.remove(id) {
            return false;
        }
        if !self.tell_agent(session, START_OVER.into()) {
            self.redispatch(id, "its agent couldn't start over, so it starts again in a fresh workspace");
        }
        true
    }

    pub(super) fn redispatch(&mut self, id: &str, why: &str) {
        let attempts = self.tasks.get(id).map_or(0, |t| t.attempts);
        if attempts >= machine::MAX_RESTARTS {
            self.hold(id, FailureClass::Timeout, format!("{why}, but it has been started {attempts} times already"));
            return;
        }
        if let Some(t) = self.tasks.get_mut(id) {
            t.attempts += 1;
            t.session = None;
        }
        self.note(id, "tessel", why);
        if self.advance(id, TaskState::Ready, Kind::Recovered, why).is_ok() {
            self.dispatch();
        }
    }

    fn watchdog_tick(&mut self) {
        let now = now_ms();
        let w = self.settings.watchdog.clone();
        let watched: Vec<(String, TaskState, Option<SessionId>, Millis)> = self
            .tasks
            .values()
            .filter(|t| matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking))
            .filter(|t| !self.shooting.contains(&t.id) && !self.redo.contains(&t.id))
            .filter(|t| t.session.and_then(|s| self.sessions.get(&s)).is_none_or(|l| l.state.pending().is_empty()))
            .filter(|t| self.retries.get(&t.id).is_none_or(|r| r.due.is_none()))
            .map(|t| (t.id.clone(), t.state, t.session, self.state_since.get(&t.id).copied().unwrap_or(t.updated_ms)))
            .collect();
        for (id, state, session, since) in watched {
            let active = session.and_then(|s| self.sessions.get(&s)).map_or(0, |l| l.state.last_output_ms());
            let mut mark = self.watch.get(&id).copied().unwrap_or_default();
            if mark.nudged.is_some_and(|n| active > n + 1000) {
                mark.nudged = None;
            }
            self.watch.insert(id.clone(), mark);
            let Some(bark) = machine::watchdog(state, now, since, active, mark, &w) else { continue };
            match (bark, session) {
                (Bark::Nudge, Some(s)) => {
                    self.watch.entry(id.clone()).or_default().nudged = Some(now_ms());
                    let told = self.tell_agent(s, NUDGE.into());
                    let said = if told { "nudged its agent" } else { "its agent couldn't be nudged" };
                    self.record(&id, Kind::Watchdog, format!("nothing happened for {}: {said}", machine::minutes(w.quiet_secs)));
                }
                (Bark::Nudge, None) => {
                    self.watch.entry(id.clone()).or_default().nudged = Some(now_ms());
                    self.record(&id, Kind::Watchdog, format!("its workspace hasn't come in {}", machine::minutes(w.quiet_secs)));
                }
                (Bark::Retry, Some(s)) => {
                    let attempts = self.tasks.get(&id).map_or(0, |t| t.attempts);
                    if attempts >= machine::MAX_RESTARTS {
                        self.record(&id, Kind::Watchdog, format!("its agent went quiet again after {attempts} tries"));
                        self.interrupt_session(s);
                        self.hold(&id, FailureClass::Timeout, format!("its agent kept going quiet ({attempts} tries)"));
                        continue;
                    }
                    self.watch.remove(&id);
                    let quiet = machine::minutes(w.nudge_secs);
                    let how = if self.interrupt_session(s) {
                        self.redo.insert(id.clone());
                        "interrupted its agent to start over"
                    } else if self.tell_agent(s, START_OVER.into()) {
                        "told its agent to start over"
                    } else {
                        self.record(&id, Kind::Watchdog, format!("still nothing {quiet} after the nudge, and its agent can't be reached"));
                        self.redispatch(&id, "its agent stopped answering: starting it again in a fresh workspace");
                        continue;
                    };
                    if let Some(t) = self.tasks.get_mut(&id) {
                        t.attempts += 1;
                    }
                    self.record(&id, Kind::Watchdog, format!("still nothing {quiet} after the nudge: {how} (attempt {})", attempts + 1));
                }
                (Bark::Retry, None) => {
                    self.record(&id, Kind::Watchdog, "its workspace never came");
                    self.hold(&id, FailureClass::Infra, "its workspace never came");
                }
                (Bark::RestartProof, _) => {
                    self.watch.entry(id.clone()).or_default().restarted = Some(now_ms());
                    match session.filter(|s| self.sessions.contains_key(s)) {
                        Some(s) => {
                            self.proof_floor.insert(s, now_ms());
                            self.proving.remove(&s);
                            self.record(&id, Kind::Watchdog, format!("its proof ran over {}: started it again", machine::minutes(w.proving_secs)));
                            self.start_proof(s, false, false);
                        }
                        None => {
                            self.record(&id, Kind::Watchdog, format!("its proof ran over {} and its workspace is gone", machine::minutes(w.proving_secs)));
                            self.hold(&id, FailureClass::EnvBlocked, "its workspace is gone, so it can't be proved");
                        }
                    }
                }
                (Bark::Hold, _) => {
                    let (what, secs) = match state {
                        TaskState::Proving => ("its proof ran over twice", w.proving_secs * 2),
                        TaskState::Landing => ("landing it took too long", w.landing_secs),
                        _ => ("its checks on GitHub took too long", w.checking_secs),
                    };
                    let why = format!("{what} ({})", machine::minutes(secs));
                    self.record(&id, Kind::Watchdog, format!("{why}: held it"));
                    if state == TaskState::Landing && self.landing.as_deref() == Some(id.as_str()) {
                        self.landing = None;
                    }
                    if state == TaskState::Checking {
                        self.ci_busy.remove(&id);
                    }
                    self.hold(&id, FailureClass::Timeout, why);
                    self.pump_land();
                }
            }
        }
    }

    pub(super) fn factory_tick(&mut self) {
        if self.settings.paused {
            return;
        }
        self.retry_tick();
        self.watchdog_tick();
    }

    pub(super) fn recover_tasks(&mut self) {
        let building: Vec<String> = self.tasks.values().filter(|t| t.building.is_some()).map(|t| t.id.clone()).collect();
        for id in building {
            self.note(&id, "tessel", "tessel restarted while it tried the options");
            self.record(&id, Kind::Recovered, "tessel restarted while it tried the options: showing what was tried");
            self.swatch_done(&id);
            self.task_saved(&id);
        }
        let open: Vec<(String, TaskState)> = self.tasks.values().filter(|t| !t.state.finished()).map(|t| (t.id.clone(), t.state)).collect();
        for (id, state) in open {
            let session = self.tasks.get(&id).and_then(|t| t.session);
            let live = session.filter(|s| self.sessions.get(s).is_some_and(|l| std::path::Path::new(&l.state.meta.cwd).is_dir()));
            match state {
                TaskState::Running => self.recover_running(&id, live),
                TaskState::Proving => match live {
                    Some(s) => {
                        self.record(&id, Kind::Recovered, "tessel restarted while it was being proved: proving it again");
                        if !self.proving.contains(&s) {
                            self.start_proof(s, false, false);
                        }
                    }
                    None => self.escalate(&id, Some(FailureClass::EnvBlocked), "tessel restarted while it was being checked, and its workspace is gone".into()),
                },
                TaskState::Landing => match live {
                    Some(_) => {
                        self.record(&id, Kind::Recovered, "tessel restarted while it was landing: checking whether it landed before landing it again");
                        self.relanding.insert(id.clone());
                        if !self.land_queue.contains(&id) {
                            self.land_queue.push_back(id.clone());
                        }
                    }
                    None => self.hold(&id, FailureClass::EnvBlocked, "tessel restarted while it was landing, and its workspace is gone"),
                },
                TaskState::Checking => {
                    self.ci_polled.remove(&id);
                    self.record(&id, Kind::Recovered, "tessel restarted while its checks ran: asking GitHub again");
                }
                TaskState::Planned => self.parent_progress(&id),
                _ => {}
            }
        }
        self.recheck_waiting();
        self.dispatch();
        self.pump_land();
    }

    fn recover_running(&mut self, id: &str, live: Option<SessionId>) {
        let Some(s) = live else {
            self.redispatch(id, "tessel restarted while it ran, and its workspace is gone: starting it again in a fresh one");
            return;
        };
        if self.sessions.get(&s).is_some_and(|l| l.state.turn_open()) {
            self.record(id, Kind::Recovered, "tessel restarted while it ran: its agent is still at work");
            return;
        }
        if self.shooting.contains(id) {
            return;
        }
        let last = self.tasks.get(id).and_then(|t| t.notes.iter().rev().find(|n| n.by == "tessel")).map(|n| n.text.clone());
        if self.settings.paused {
            if last.as_deref() != Some(tasks::WAITING) && last.as_deref() != Some(tasks::PAUSED) {
                self.note(id, "tessel", tasks::PAUSED);
            }
            self.record(id, Kind::Recovered, "tessel restarted while it ran: it carries on when the factory resumes");
            return;
        }
        if last.as_deref() == Some(tasks::WAITING) {
            self.record(id, Kind::Recovered, "tessel restarted after its agent finished: proving it");
            if self.advance(id, TaskState::Proving, Kind::TurnEnded, "its agent had finished: proving it").is_ok() && !self.proving.contains(&s) {
                self.start_proof(s, false, false);
            }
            return;
        }
        if self.tell_agent(s, CARRY_ON.into()) {
            self.record(id, Kind::Recovered, "tessel restarted while its agent worked: told it to carry on");
        } else {
            self.redispatch(id, "tessel restarted while it ran, and its agent couldn't carry on: starting it again in a fresh workspace");
        }
    }

    pub(super) fn errored_turn(&mut self, id: &str, session: SessionId) {
        if !self.retry_or(id, FailureClass::ProviderTransient, "the agent stopped with an error", Retry::Tell { session, text: AFTER_ERROR.into(), at: TaskState::Running, to: TaskState::Running }) {
            self.hold(id, FailureClass::ProviderTransient, "the agent stopped with an error");
        }
    }

    pub(super) fn task_events(&mut self, client: ClientId, id: u64, since: Option<i64>, task: Option<String>, limit: u32) {
        let (tx, store) = (self.tx.clone(), self.store.clone());
        tokio::spawn(async move {
            let result = store.task_events(since, task, limit).await.map(|events| Response::TaskEvents { events }).map_err(|e| RpcError::new(ErrorCode::Internal, format!("couldn't read the task log: {e:#}")));
            let _ = tx.send(HubMsg::Reply { client, id, result });
        });
    }
}
