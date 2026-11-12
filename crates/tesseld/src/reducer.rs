//! what a session is, as a pure state machine.
//!
//! `apply(state, input, now) -> effects`. no io, no clock of its own, no
//! knowledge of claude. the daemon feeds it and acts on what comes back;
//! tests feed it recorded sessions.
//!
//! the two ideas that matter:
//!
//! 1. **run state is derived, never stored.** `run_state()` looks at facts
//!    (is something waiting on the user, is a turn open, is a tool in flight,
//!    how long since the agent last spoke) every time it's asked. so one
//!    subagent starting a tool can't hide that another is blocked on you.
//!
//! 2. **attention is latched.** when a session starts wanting you it keeps
//!    wanting you until you look (or until the reason goes away). that is what
//!    makes "it finished 20 minutes ago and nothing told me" impossible.

use crate::activity::{summarize_tool, truncate};
use tessel_proto::{McpServer, TaskSummary, 
    AgentEvent, AgentPhase, Attention, AttentionReason, BlockKind, Item, ItemBody, ItemId, Millis,
    NoticeLevel, PermissionDecision, PermissionRequest, RequestId, RunState, Seq, SessionId,
    SessionSnapshot, SessionSummary, ToolStatus, TurnOutcome, WorktreeInfo,
};
use tessel_proto::{TRACE_BUCKET_MS, TRACE_BUCKETS};
use std::collections::{HashMap, HashSet, VecDeque};

/// a turn with nothing in flight that has been silent this long is "stalled".
/// a running tool is never a stall, however long it takes.
pub const STALL_MS: Millis = 120_000;

/// tool output kept on the item. the full output is always in the raw log.
pub const MAX_OUTPUT: usize = 48 * 1024;

#[derive(Clone, Debug)]
pub enum Input {
    /// an agent process is being started for this session.
    Spawning,
    /// the user sent a message and we passed it to the agent.
    UserSent { item_id: ItemId, text: String, attachments: Vec<String> },
    /// the user picked a model or mode while no agent was running. it takes
    /// effect when the next one starts.
    SetModel { model: String },
    SetEffort { effort: Option<String> },
    SetFast { fast: bool },
    SetVisual { visual: Option<bool>, bash: Option<bool> },
    SetMode { mode: String },
    LinkDir { path: String },
    UnlinkDir { path: String },
    /// someone answered a pending request.
    Answered { request_id: RequestId, decision: PermissionDecision },
    /// a client has looked at the session up to `seq`.
    MarkSeen { seq: Seq },
    SetGoal { goal: String },
    Agent(AgentEvent),
    /// the agent wrote something we don't turn into an event. still proof of life.
    Alive,
    /// the daemon came up and found this session in its store. whatever was
    /// running when it went down is gone.
    Recovered,
    /// the agent drew, changed or removed a surface. checked by the hub first.
    Surface(tessel_proto::surface::SurfaceOp),
    /// a running agent was taken over from the previous daemon. `cost_process`:
    /// what this process had cost so far, already in the stored total.
    Adopted { cost_process: f64 },
}

/// what changed, for the daemon to persist and broadcast.
#[derive(Debug, Default)]
pub struct Effects {
    /// items created or changed, in their new form.
    pub upserts: Vec<Item>,
    /// streaming text. never persisted.
    pub deltas: Vec<Delta>,
    pub pending_changed: bool,
    /// the summary may differ from before.
    pub summary_changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub item_id: ItemId,
    /// byte offset within the item's text where `text` starts.
    pub offset: u64,
    pub text: String,
}

/// the parts of a session that never come from the agent.
#[derive(Clone, Debug, PartialEq)]
pub struct Meta {
    pub id: SessionId,
    pub goal: String,
    pub cwd: String,
    pub worktree: Option<WorktreeInfo>,
    pub parent: Option<SessionId>,
    pub created_ms: Millis,
    /// other folders the agent may work in besides `cwd`.
    pub linked_dirs: Vec<String>,
}

#[derive(Clone, Debug)]
struct Task {
    id: String,
    tool_item: Option<ItemId>,
    description: String,
    background: bool,
    since_ms: Millis,
}

#[derive(Clone, Debug)]
pub struct SessionState {
    pub meta: Meta,
    pub seq: Seq,
    items: Vec<Item>,
    index: HashMap<ItemId, usize>,
    /// items that are still streaming or are tools without a result.
    open: HashSet<ItemId>,
    pending: Vec<PermissionRequest>,
    /// tool calls the user refused, so their failed result reads as "denied".
    denied: HashSet<ItemId>,
    /// our own messages the agent hasn't echoed back yet.
    unechoed: VecDeque<ItemId>,
    tasks: Vec<Task>,
    turn_open: bool,
    starting: bool,
    alive: bool,
    errored: bool,
    /// `Some(reset)` while the agent says we're over a usage limit.
    limited: Option<Option<i64>>,
    last_output_ms: Millis,
    /// output volume per bucket, oldest first; the last one is still filling.
    trace: [u32; TRACE_BUCKETS],
    /// when the bucket that is still filling began.
    trace_at: Millis,
    pub attention: Option<Attention>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub visual: bool,
    pub visual_bash: bool,
    context_used: Option<u64>,
    context_window: Option<u64>,
    mcp: Vec<McpServer>,
    /// the agent's own id for this conversation, used to resume it.
    pub agent_session_id: Option<String>,
    /// cost of earlier agent processes for this session.
    cost_base: f64,
    /// cost reported by the current process (it reports a running total).
    cost_process: f64,
    pub updated_ms: Millis,
}

/// the persisted half of a session, for rebuilding one from the store.
#[derive(Clone, Debug, PartialEq)]
pub struct Persisted {
    pub meta: Meta,
    pub seq: Seq,
    pub attention: Option<Attention>,
    pub model: Option<String>,
    pub permission_mode: Option<String>,
    pub effort: Option<String>,
    pub fast: bool,
    pub visual: bool,
    pub visual_bash: bool,
    pub context_used: Option<u64>,
    pub context_window: Option<u64>,
    pub agent_session_id: Option<String>,
    pub cost_usd: f64,
    /// a turn was in progress. after a restart that means it was cut short,
    /// even if nothing visible was in flight (the agent was between steps).
    pub turn_open: bool,
    pub updated_ms: Millis,
    /// the tool servers the agent last had: still shown while it isn't running.
    pub mcp: Vec<McpServer>,
}

impl SessionState {
    pub fn new(meta: Meta) -> Self {
        let created = meta.created_ms;
        Self {
            meta,
            seq: 0,
            items: Vec::new(),
            index: HashMap::new(),
            open: HashSet::new(),
            pending: Vec::new(),
            denied: HashSet::new(),
            unechoed: VecDeque::new(),
            tasks: Vec::new(),
            turn_open: false,
            starting: false,
            alive: false,
            errored: false,
            limited: None,
            last_output_ms: created,
            trace: [0; TRACE_BUCKETS],
            trace_at: created,
            attention: None,
            model: None,
            permission_mode: None,
            effort: None,
            fast: false,
            visual: false,
            visual_bash: false,
            context_used: None,
            context_window: None,
            mcp: Vec::new(),
            agent_session_id: None,
            cost_base: 0.0,
            cost_process: 0.0,
            updated_ms: created,
        }
    }

    /// rebuild from the store. call `apply(Input::Recovered, ..)` afterwards:
    /// anything that was mid-flight when the daemon stopped is not coming back.
    pub fn restore(p: Persisted, mut items: Vec<Item>) -> Self {
        items.sort_by_key(|i| i.order);
        let mut s = Self::new(p.meta);
        s.seq = p.seq;
        s.attention = p.attention;
        s.model = p.model;
        s.permission_mode = p.permission_mode;
        s.effort = p.effort;
        s.fast = p.fast;
        s.visual = p.visual;
        s.visual_bash = p.visual_bash;
        s.context_used = p.context_used;
        s.context_window = p.context_window;
        s.agent_session_id = p.agent_session_id;
        s.cost_base = p.cost_usd;
        s.turn_open = p.turn_open;
        s.updated_ms = p.updated_ms;
        s.last_output_ms = p.updated_ms;
        s.mcp = p.mcp;
        for (i, item) in items.iter().enumerate() {
            s.index.insert(item.id.clone(), i);
            if is_open(&item.body) {
                s.open.insert(item.id.clone());
            }
        }
        s.items = items;
        s
    }

    pub fn persisted(&self) -> Persisted {
        Persisted {
            meta: self.meta.clone(),
            seq: self.seq,
            attention: self.attention.clone(),
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            effort: self.effort.clone(),
            fast: self.fast,
            visual: self.visual,
            visual_bash: self.visual_bash,
            context_used: self.context_used,
            context_window: self.context_window,
            agent_session_id: self.agent_session_id.clone(),
            cost_usd: self.cost_usd(),
            turn_open: self.turn_open,
            updated_ms: self.updated_ms,
            mcp: self.mcp.clone(),
        }
    }

    /// start from another session's transcript: a fork shows the history it carries on from.
    pub fn seed_items(&mut self, items: Vec<Item>) {
        for item in items {
            self.seq = self.seq.max(item.seq);
            self.index.insert(item.id.clone(), self.items.len());
            self.items.push(item);
        }
    }

    pub fn items(&self) -> &[Item] {
        &self.items
    }

    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.index.get(id).map(|&i| &self.items[i])
    }

    pub fn pending(&self) -> &[PermissionRequest] {
        &self.pending
    }

    pub fn pending_request(&self, id: &RequestId) -> Option<&PermissionRequest> {
        self.pending.iter().find(|p| &p.request_id == id)
    }

    pub fn is_alive(&self) -> bool {
        self.alive
    }

    pub fn turn_open(&self) -> bool {
        self.turn_open
    }

    /// in the middle of something only this daemon's memory knows: a turn, a
    /// message being written, a question waiting on an answer.
    pub fn mid_turn(&self) -> bool {
        self.turn_open || self.has_open_items() || !self.pending.is_empty()
    }

    /// what the running process has cost so far.
    pub fn cost_process(&self) -> f64 {
        self.cost_process
    }

    pub fn cost_usd(&self) -> f64 {
        self.cost_base + self.cost_process
    }

    /// true while anything is unfinished: a streaming block, a tool without a result.
    pub fn has_open_items(&self) -> bool {
        !self.open.is_empty()
    }

    fn has_open_tool(&self) -> bool {
        self.open
            .iter()
            .filter_map(|id| self.item(id))
            .any(|i| matches!(i.body, ItemBody::Tool { .. }))
    }

    // ---- derived -------------------------------------------------------

    pub fn run_state(&self, now: Millis) -> RunState {
        if !self.pending.is_empty() {
            return if self.pending.iter().any(|p| !p.is_question) {
                RunState::NeedsPermission
            } else {
                RunState::NeedsInput
            };
        }
        if self.starting {
            return RunState::Starting;
        }
        if self.turn_open {
            let in_flight = self.has_open_tool() || !self.tasks.is_empty();
            if !in_flight && now.saturating_sub(self.last_output_ms) > STALL_MS {
                return RunState::Stalled;
            }
            return RunState::Working;
        }
        if let Some(reset_at) = self.limited {
            return RunState::Limited { reset_at };
        }
        if self.errored {
            return RunState::Errored;
        }
        RunState::Idle
    }

    /// the live "currently doing" line and when it started.
    pub fn activity(&self) -> Option<(String, Millis)> {
        if let Some(p) = self.pending.first() {
            let since = self.attention.as_ref().map_or(self.updated_ms, |a| a.since_ms);
            let what = summarize_tool(&p.tool, &p.input);
            let line = if p.plan.is_some() {
                "has a plan for you to review".to_owned()
            } else if p.is_question {
                "asked you a question".to_owned()
            } else {
                format!("wants to: {what}")
            };
            return Some((truncate(&line), since));
        }
        // the newest unfinished thing is the best description of "now".
        let newest_open = self
            .open
            .iter()
            .filter_map(|id| self.item(id))
            .max_by_key(|i| i.order);
        if let Some(item) = newest_open {
            match &item.body {
                ItemBody::Tool { tool, input, started_ms, .. } => {
                    let task = self.tasks.iter().find(|t| t.tool_item.as_ref() == Some(&item.id));
                    let line = match task {
                        Some(t) if !t.description.is_empty() => format!("subagent: {}", t.description),
                        _ => summarize_tool(tool, input),
                    };
                    return Some((truncate(&line), started_ms.unwrap_or(item.created_ms)));
                }
                ItemBody::Text { .. } => return Some(("writing".to_owned(), item.created_ms)),
                ItemBody::Thinking { .. } => return Some(("thinking".to_owned(), item.created_ms)),
                _ => {}
            }
        }
        if self.turn_open {
            return Some(("thinking".to_owned(), self.last_output_ms));
        }
        self.tasks
            .iter()
            .rfind(|t| t.background)
            .map(|t| (truncate(&format!("background: {}", t.description)), t.since_ms))
    }

    /// the trace as of `now`, oldest first. empty if the whole window is silent.
    pub fn trace(&self, now: Millis) -> Vec<u16> {
        let shift = (now.saturating_sub(self.trace_at) / TRACE_BUCKET_MS).min(TRACE_BUCKETS as u64) as usize;
        let mut out: Vec<u16> = self.trace[shift..].iter().map(|v| (*v).min(u32::from(u16::MAX)) as u16).collect();
        out.resize(TRACE_BUCKETS, 0);
        if out.iter().all(|v| *v == 0) { Vec::new() } else { out }
    }

    /// count `amount` of output at `now`.
    fn record(&mut self, amount: u32, now: Millis) {
        let shift = (now.saturating_sub(self.trace_at) / TRACE_BUCKET_MS) as usize;
        if shift > 0 {
            if shift >= TRACE_BUCKETS {
                self.trace = [0; TRACE_BUCKETS];
            } else {
                self.trace.rotate_left(shift);
                for v in &mut self.trace[TRACE_BUCKETS - shift..] {
                    *v = 0;
                }
            }
            self.trace_at += shift as Millis * TRACE_BUCKET_MS;
        }
        let newest = &mut self.trace[TRACE_BUCKETS - 1];
        *newest = newest.saturating_add(amount);
    }

    pub fn summary(&self, now: Millis) -> SessionSummary {
        let activity = self.activity();
        SessionSummary {
            id: self.meta.id,
            goal: self.meta.goal.clone(),
            cwd: self.meta.cwd.clone(),
            worktree: self.meta.worktree.clone(),
            parent: self.meta.parent,
            run_state: self.run_state(now),
            activity: activity.as_ref().map(|a| a.0.clone()),
            activity_since_ms: activity.map(|a| a.1),
            attention: self.attention.clone(),
            warm: self.alive,
            background_tasks: self.tasks.iter().filter(|t| t.background).count() as u32,
            model: self.model.clone(),
            permission_mode: self.permission_mode.clone(),
            effort: self.effort.clone(),
            fast: self.fast,
            visual: self.visual,
            visual_bash: self.visual_bash,
            context_used: self.context_used,
            context_window: self.context_window,
            mcp: self.mcp.clone(),
            linked_dirs: self.meta.linked_dirs.clone(),
            tasks: self
                .tasks
                .iter()
                .map(|t| TaskSummary { id: t.id.clone(), description: t.description.clone(), background: t.background, item: t.tool_item.clone() })
                .collect(),
            cost_usd: self.cost_usd(),
            trace: self.trace(now),
            asking: self.pending.iter().find(|p| !p.is_question && p.plan.is_none()).map(|p| p.request_id.clone()),
            last_seq: self.seq,
            created_ms: self.meta.created_ms,
            updated_ms: self.updated_ms,
        }
    }

    pub fn snapshot(&self, tail: usize, now: Millis) -> SessionSnapshot {
        let start = self.items.len().saturating_sub(tail);
        SessionSnapshot {
            summary: self.summary(now),
            items: self.items[start..].to_vec(),
            has_more: start > 0,
            pending: self.pending.clone(),
        }
    }

    /// up to `limit` items that come before `before` in the transcript, oldest first.
    pub fn items_before(&self, before: Seq, limit: usize) -> (Vec<Item>, bool) {
        let end = self.items.partition_point(|i| i.order < before);
        let start = end.saturating_sub(limit);
        (self.items[start..end].to_vec(), start > 0)
    }

    // ---- transitions ---------------------------------------------------

    pub fn apply(&mut self, input: Input, now: Millis) -> Effects {
        let mut fx = Effects::default();
        let before = (self.seq, self.attention.clone());
        match input {
            Input::Adopted { cost_process } => {
                self.alive = true;
                self.starting = false;
                self.cost_base = (self.cost_base - cost_process).max(0.0);
                self.cost_process = cost_process;
                self.last_output_ms = now;
                fx.summary_changed = true;
            }
            Input::Spawning => {
                self.starting = true;
                self.alive = true;
                self.last_output_ms = now;
                fx.summary_changed = true;
            }
            Input::SetModel { model } => {
                if self.model.as_deref() != Some(&model) {
                    self.model = Some(model);
                    fx.summary_changed = true;
                }
            }
            Input::SetEffort { effort } => {
                if self.effort != effort {
                    self.effort = effort;
                    fx.summary_changed = true;
                }
            }
            Input::SetVisual { visual, bash } => {
                let (v, b) = (visual.unwrap_or(self.visual), bash.unwrap_or(self.visual_bash));
                if (v, b) != (self.visual, self.visual_bash) {
                    (self.visual, self.visual_bash) = (v, b);
                    fx.summary_changed = true;
                }
            }
            Input::SetFast { fast } => {
                if self.fast != fast {
                    self.fast = fast;
                    fx.summary_changed = true;
                }
            }
            Input::LinkDir { path } => {
                if !self.meta.linked_dirs.contains(&path) {
                    self.meta.linked_dirs.push(path);
                    fx.summary_changed = true;
                }
            }
            Input::UnlinkDir { path } => {
                let n = self.meta.linked_dirs.len();
                self.meta.linked_dirs.retain(|d| d != &path);
                fx.summary_changed |= self.meta.linked_dirs.len() != n;
            }
            Input::SetMode { mode } => {
                if self.permission_mode.as_deref() != Some(&mode) {
                    self.permission_mode = Some(mode);
                    fx.summary_changed = true;
                }
            }
            Input::Surface(op) => {
                use tessel_proto::surface::{SurfaceOp, set_props};
                let id = ItemId::from(format!("surface/{}", op.surface()).as_str());
                let body = match op {
                    SurfaceOp::Put { surface, node, region } => Some(ItemBody::Surface { surface, node, region }),
                    SurfaceOp::Set { surface, element, props } => match self.item(&id).map(|i| &i.body) {
                        Some(ItemBody::Surface { node, region, .. }) => {
                            let (mut node, region) = (node.clone(), region.clone());
                            set_props(&mut node, &element, &props).then_some(ItemBody::Surface { surface, node, region })
                        }
                        _ => None,
                    },
                    SurfaceOp::Remove { surface } => match self.item(&id).map(|i| &i.body) {
                        Some(ItemBody::Surface { region, .. }) => Some(ItemBody::Surface { surface, node: serde_json::Value::Null, region: region.clone() }),
                        _ => None,
                    },
                };
                if let Some(body) = body {
                    self.upsert(id, None, body, now, &mut fx);
                }
            }
            Input::UserSent { item_id, text, attachments } => {
                self.unechoed.push_back(item_id.clone());
                self.upsert(item_id, None, ItemBody::User { text, attachments }, now, &mut fx);
                self.begin_turn(now);
                // you're clearly here: a "finished"/"errored" flag has done its job.
                if matches!(self.attention, Some(Attention { reason: AttentionReason::Finished | AttentionReason::Errored, .. })) {
                    self.attention = None;
                }
            }
            Input::Answered { request_id, decision } => {
                // idempotent: answering something already answered is a no-op.
                if let Some(pos) = self.pending.iter().position(|p| p.request_id == request_id) {
                    let req = self.pending.remove(pos);
                    fx.pending_changed = true;
                    self.seq += 1;
                    if let Some(tool) = req.tool_item {
                        let status = match decision {
                            PermissionDecision::Allow { .. } => ToolStatus::Running,
                            PermissionDecision::Deny { .. } => {
                                self.denied.insert(tool.clone());
                                ToolStatus::Denied
                            }
                        };
                        self.set_tool_status(&tool, status, None, now, &mut fx);
                    }
                    self.sync_blocked_attention(now);
                }
            }
            Input::MarkSeen { seq } => {
                if let Some(a) = &self.attention
                    && matches!(a.reason, AttentionReason::Finished | AttentionReason::Errored)
                    && a.seq <= seq
                {
                    self.attention = None;
                }
            }
            Input::SetGoal { goal } => {
                if self.meta.goal != goal {
                    self.meta.goal = goal;
                    self.seq += 1;
                }
            }
            Input::Alive => {
                self.last_output_ms = now;
                if self.starting {
                    self.starting = false;
                    fx.summary_changed = true;
                }
            }
            Input::Recovered => self.process_gone(now, &mut fx),
            Input::Agent(ev) => self.agent(ev, now, &mut fx),
        }
        if before.0 != self.seq || before.1 != self.attention {
            fx.summary_changed = true;
        }
        if fx.summary_changed || !fx.upserts.is_empty() || fx.pending_changed {
            self.updated_ms = now;
        }
        fx
    }

    fn agent(&mut self, ev: AgentEvent, now: Millis, fx: &mut Effects) {
        // what counts as output: text by its length, and each step the agent
        // takes (a tool call, a result) as a fixed small amount, so a turn
        // that is all tool calls still shows as alive.
        match &ev {
            AgentEvent::Delta { text, .. } => self.record(text.len() as u32, now),
            AgentEvent::ToolCall { .. } | AgentEvent::ToolResult { .. } | AgentEvent::TaskProgress { .. } => self.record(48, now),
            _ => {}
        }
        if !matches!(ev, AgentEvent::Exited { .. }) {
            self.last_output_ms = now;
            if self.starting {
                self.starting = false;
                fx.summary_changed = true;
            }
        }
        match ev {
            AgentEvent::McpServers { servers } => {
                if self.mcp != servers {
                    self.mcp = servers;
                    fx.summary_changed = true;
                }
            }
            AgentEvent::Init { agent_session_id, model, permission_mode, mcp_servers, .. } => {
                if !mcp_servers.is_empty() {
                    self.mcp = mcp_servers;
                }
                self.alive = true;
                if !agent_session_id.is_empty() {
                    self.agent_session_id = Some(agent_session_id);
                }
                if model.is_some() {
                    self.model = model;
                }
                if permission_mode.is_some() {
                    self.permission_mode = permission_mode;
                }
                fx.summary_changed = true;
            }
            AgentEvent::ModelChanged { model } => {
                if self.model.as_deref() != Some(&model) {
                    self.model = Some(model);
                    // a different model, a different window: the old size no longer applies.
                    self.context_window = None;
                    fx.summary_changed = true;
                }
            }
            AgentEvent::ModeChanged { mode } => {
                if self.permission_mode.as_deref() != Some(&mode) {
                    self.permission_mode = Some(mode);
                    fx.summary_changed = true;
                }
            }
            AgentEvent::ContextUsed { tokens } => {
                if self.context_used != Some(tokens) {
                    self.context_used = Some(tokens);
                    fx.summary_changed = true;
                }
            }
            AgentEvent::ContextWindow { tokens } => {
                if self.context_window != Some(tokens) {
                    self.context_window = Some(tokens);
                    fx.summary_changed = true;
                }
            }
            AgentEvent::RequestFailed { message } => self.notice(NoticeLevel::Warn, message, now, fx),
            AgentEvent::Phase { phase } => match phase {
                // a turn can start with nobody asking: a background task
                // finishing wakes the agent up.
                AgentPhase::Running => {
                    if !self.turn_open {
                        self.begin_turn(now);
                        fx.summary_changed = true;
                    }
                }
                AgentPhase::Idle => {
                    if self.turn_open {
                        self.end_turn(None, now, fx);
                    }
                }
                AgentPhase::RequiresAction | AgentPhase::Unknown => {}
            },
            AgentEvent::UserMessage { item_id, text } => {
                // our own message coming back: we already showed it.
                if self.unechoed.pop_front().is_none() {
                    self.upsert(item_id, None, ItemBody::User { text, attachments: Vec::new() }, now, fx);
                }
                if !self.turn_open {
                    self.begin_turn(now);
                }
            }
            AgentEvent::BlockStarted { item_id, parent, block } => {
                if !self.turn_open {
                    self.begin_turn(now);
                }
                let body = match block {
                    BlockKind::Text => ItemBody::Text { text: String::new(), streaming: true },
                    BlockKind::Thinking => ItemBody::Thinking { text: String::new(), streaming: true },
                    BlockKind::Tool { tool } => ItemBody::Tool {
                        tool,
                        input: serde_json::Value::Null,
                        status: ToolStatus::Pending,
                        output: None,
                        started_ms: None,
                        finished_ms: None,
                    },
                    BlockKind::Unknown => return,
                };
                self.upsert(item_id, parent, body, now, fx);
            }
            AgentEvent::Delta { item_id, text } => {
                let Some(&i) = self.index.get(&item_id) else { return };
                if let ItemBody::Text { text: t, streaming: true } | ItemBody::Thinking { text: t, streaming: true } =
                    &mut self.items[i].body
                {
                    let offset = t.len() as u64;
                    t.push_str(&text);
                    fx.deltas.push(Delta { item_id, offset, text });
                }
            }
            AgentEvent::Text { item_id, parent, text } => {
                self.upsert(item_id, parent, ItemBody::Text { text, streaming: false }, now, fx);
            }
            AgentEvent::Thinking { item_id, parent, text } => {
                self.upsert(item_id, parent, ItemBody::Thinking { text, streaming: false }, now, fx);
            }
            AgentEvent::ToolCall { item_id, parent, tool, input } => {
                let waiting = self.pending.iter().any(|p| p.tool_item.as_ref() == Some(&item_id));
                let status = if waiting { ToolStatus::AwaitingPermission } else { ToolStatus::Running };
                // no prompt pending for it: it is running as of now.
                let started_ms = (status == ToolStatus::Running).then_some(now);
                self.upsert(item_id, parent, ItemBody::Tool { tool, input, status, output: None, started_ms, finished_ms: None }, now, fx);
            }
            AgentEvent::ToolResult { item_id, ok, output } => {
                let status = if ok {
                    ToolStatus::Ok
                } else if self.denied.contains(&item_id) {
                    ToolStatus::Denied
                } else {
                    ToolStatus::Failed
                };
                self.set_tool_status(&item_id, status, Some(cap_output(output)), now, fx);
            }
            AgentEvent::PermissionRequested { request } => {
                if self.pending.iter().any(|p| p.request_id == request.request_id) {
                    return;
                }
                if let Some(tool) = request.tool_item.clone() {
                    self.set_tool_status(&tool, ToolStatus::AwaitingPermission, None, now, fx);
                }
                self.pending.push(request);
                self.seq += 1;
                fx.pending_changed = true;
                self.sync_blocked_attention(now);
            }
            AgentEvent::PermissionCancelled { request_id } => {
                if let Some(pos) = self.pending.iter().position(|p| p.request_id == request_id) {
                    self.pending.remove(pos);
                    self.seq += 1;
                    fx.pending_changed = true;
                    self.sync_blocked_attention(now);
                }
            }
            AgentEvent::TaskStarted { task_id, tool_item, description, background } => {
                if !self.tasks.iter().any(|t| t.id == task_id) {
                    self.tasks.push(Task { id: task_id, tool_item, description, background, since_ms: now });
                    fx.summary_changed = true;
                }
            }
            AgentEvent::TaskProgress { task_id, description } => {
                if let Some(t) = self.tasks.iter_mut().find(|t| t.id == task_id)
                    && !description.is_empty()
                    && t.description != description
                {
                    t.description = description;
                    fx.summary_changed = true;
                }
            }
            AgentEvent::TaskFinished { task_id, .. } => {
                let n = self.tasks.len();
                self.tasks.retain(|t| t.id != task_id);
                fx.summary_changed |= self.tasks.len() != n;
            }
            AgentEvent::TurnFinished { outcome, cost_usd, duration_ms, usage } => {
                // the agent reports what its process has cost so far; the turn is the step.
                let turn_cost = cost_usd.map(|c| (c - self.cost_process).max(0.0));
                if let Some(c) = cost_usd {
                    self.cost_process = c;
                }
                self.end_turn(Some(outcome), now, fx);
                self.turn_item(turn_cost, duration_ms, usage, now, fx);
                fx.summary_changed = true;
            }
            AgentEvent::RateLimit { status, resets_at, .. } => {
                let limited = (status == "rejected").then_some(resets_at);
                if limited != self.limited {
                    self.limited = limited;
                    fx.summary_changed = true;
                }
            }
            AgentEvent::Exited { .. } => self.process_gone(now, fx),
            AgentEvent::Unknown => {}
        }
    }

    fn begin_turn(&mut self, now: Millis) {
        self.turn_open = true;
        self.errored = false;
        self.last_output_ms = now;
    }

    /// close a turn. `None` is the agent going idle without saying how the
    /// turn ended; we tidy up but don't raise anything.
    fn end_turn(&mut self, outcome: Option<TurnOutcome>, now: Millis, fx: &mut Effects) {
        self.turn_open = false;
        self.close_open_items(now, fx);
        if !self.pending.is_empty() {
            // the turn is over; nothing is waiting on these any more.
            self.pending.clear();
            self.seq += 1;
            fx.pending_changed = true;
        }
        // drop "blocked on you" first: it outranks everything, and left in
        // place it would swallow the error below and then be cleared itself,
        // leaving a cut-off session with no flag at all.
        self.sync_blocked_attention(now);
        match outcome {
            Some(TurnOutcome::Completed) => self.raise(AttentionReason::Finished, now),
            Some(TurnOutcome::Interrupted) => {
                self.notice(NoticeLevel::Info, "interrupted".into(), now, fx);
            }
            Some(TurnOutcome::Error { message }) => {
                self.errored = true;
                self.notice(NoticeLevel::Error, message, now, fx);
                self.raise(AttentionReason::Errored, now);
            }
            Some(TurnOutcome::Aborted) => {
                self.errored = true;
                self.notice(NoticeLevel::Error, "the agent stopped before this turn finished".into(), now, fx);
                self.raise(AttentionReason::Errored, now);
            }
            Some(TurnOutcome::Unknown) | None => {}
        }
    }

    /// the agent process is not there any more (it exited, or the daemon restarted).
    fn process_gone(&mut self, now: Millis, fx: &mut Effects) {
        self.alive = false;
        self.starting = false;
        self.cost_base += self.cost_process;
        self.cost_process = 0.0;
        self.unechoed.clear();
        if !self.tasks.is_empty() {
            self.tasks.clear();
        }
        fx.summary_changed = true;
        if self.turn_open || self.has_open_items() || !self.pending.is_empty() {
            self.end_turn(Some(TurnOutcome::Aborted), now, fx);
        }
    }

    fn close_open_items(&mut self, now: Millis, fx: &mut Effects) {
        let mut ids: Vec<ItemId> = self.open.drain().collect();
        ids.sort_by_key(|id| self.index.get(id).copied());
        for id in ids {
            let Some(&i) = self.index.get(&id) else { continue };
            match &mut self.items[i].body {
                ItemBody::Text { streaming, .. } | ItemBody::Thinking { streaming, .. } => *streaming = false,
                ItemBody::Tool { status, finished_ms, .. } if status.is_open() => {
                    *status = ToolStatus::Aborted;
                    *finished_ms = Some(now);
                }
                _ => continue,
            }
            self.seq += 1;
            self.items[i].seq = self.seq;
            fx.upserts.push(self.items[i].clone());
        }
    }

    fn upsert(&mut self, id: ItemId, parent: Option<ItemId>, body: ItemBody, now: Millis, fx: &mut Effects) {
        self.seq += 1;
        let open = is_open(&body);
        let i = match self.index.get(&id) {
            Some(&i) => {
                let item = &mut self.items[i];
                item.body = body;
                item.seq = self.seq;
                if parent.is_some() {
                    item.parent = parent;
                }
                i
            }
            None => {
                self.items.push(Item { id: id.clone(), parent, seq: self.seq, order: self.seq, created_ms: now, body });
                self.index.insert(id.clone(), self.items.len() - 1);
                self.items.len() - 1
            }
        };
        if open {
            self.open.insert(id);
        } else {
            self.open.remove(&id);
        }
        fx.upserts.push(self.items[i].clone());
    }

    fn set_tool_status(&mut self, id: &ItemId, new: ToolStatus, out: Option<String>, now: Millis, fx: &mut Effects) {
        let Some(&i) = self.index.get(id) else { return };
        let ItemBody::Tool { status, output, started_ms, finished_ms, .. } = &mut self.items[i].body else { return };
        if *status == new && out.is_none() {
            return;
        }
        *status = new;
        match new {
            // the call usually arrives just before its prompt, so it briefly
            // looked like it was running. it was not: stop the clock.
            ToolStatus::AwaitingPermission => *started_ms = None,
            // permission was just granted: the clock starts here.
            ToolStatus::Running if started_ms.is_none() => *started_ms = Some(now),
            _ => {}
        }
        if let Some(o) = out {
            *output = Some(o);
        }
        if new.is_open() {
            self.open.insert(id.clone());
        } else {
            *finished_ms = Some(now);
            self.open.remove(id);
        }
        self.seq += 1;
        self.items[i].seq = self.seq;
        fx.upserts.push(self.items[i].clone());
    }

    /// the turn that just ended, as an item: from the message that started it
    /// to now, with what it cost and took, and the files its edits touched.
    fn turn_item(&mut self, cost_usd: Option<f64>, duration_ms: Option<u64>, usage: Option<tessel_proto::TurnUsage>, now: Millis, fx: &mut Effects) {
        let start = self.items.iter().rposition(|i| matches!(&i.body, ItemBody::User { text, .. } if !text.starts_with('<'))).unwrap_or(0);
        let started_ms = self.items.get(start).map_or(now, |i| i.created_ms);
        let lines = |t: &str| if t.is_empty() { 0 } else { t.lines().count() as u32 };
        let mut files: Vec<tessel_proto::TurnFile> = Vec::new();
        for item in &self.items[start..] {
            let ItemBody::Tool { tool, input, status: ToolStatus::Ok, .. } = &item.body else { continue };
            let s = |k: &str| input.get(k).and_then(serde_json::Value::as_str).unwrap_or("");
            let (path, added, removed) = match tool.as_str() {
                "Edit" => (s("file_path"), lines(s("new_string")), lines(s("old_string"))),
                "Write" => (s("file_path"), lines(s("content")), 0),
                "MultiEdit" => {
                    let edits = input.get("edits").and_then(serde_json::Value::as_array).cloned().unwrap_or_default();
                    let (mut a, mut r) = (0, 0);
                    for e in &edits {
                        a += lines(e.get("new_string").and_then(serde_json::Value::as_str).unwrap_or(""));
                        r += lines(e.get("old_string").and_then(serde_json::Value::as_str).unwrap_or(""));
                    }
                    (s("file_path"), a, r)
                }
                "NotebookEdit" => (s("notebook_path"), lines(s("new_source")), 0),
                _ => continue,
            };
            if path.is_empty() {
                continue;
            }
            // shown relative to the session's folder when it's inside it.
            let cwd = self.meta.cwd.trim_end_matches('/');
            let shown = path.strip_prefix(cwd).map(|p| p.trim_start_matches('/')).filter(|p| !p.is_empty()).unwrap_or(path).to_owned();
            match files.iter_mut().find(|f| f.path == shown) {
                Some(f) => {
                    f.added += added;
                    f.removed += removed;
                }
                None => files.push(tessel_proto::TurnFile { path: shown, added, removed }),
            }
        }
        let id = ItemId(format!("turn/{}", self.seq + 1));
        let body = ItemBody::Turn { started_ms, finished_ms: now, duration_ms, cost_usd, model: self.model.clone(), fast: self.fast, usage, files };
        self.upsert(id, None, body, now, fx);
    }

    fn notice(&mut self, level: NoticeLevel, text: String, now: Millis, fx: &mut Effects) {
        let id = ItemId(format!("notice/{}", self.seq + 1));
        self.upsert(id, None, ItemBody::Notice { level, text }, now, fx);
    }

    fn raise(&mut self, reason: AttentionReason, now: Millis) {
        match &self.attention {
            Some(a) if a.reason == reason => {}
            Some(a) if rank(a.reason) > rank(reason) => {}
            Some(a) if rank(a.reason) == rank(reason) => {
                // one kind of "blocked" turning into the other: same wait, keep its start.
                self.attention = Some(Attention { reason, since_ms: a.since_ms, seq: self.seq });
            }
            _ => self.attention = Some(Attention { reason, since_ms: now, seq: self.seq }),
        }
    }

    /// keep "blocked on you" attention in step with what is actually pending.
    fn sync_blocked_attention(&mut self, now: Millis) {
        let blocked = matches!(
            self.attention,
            Some(Attention { reason: AttentionReason::NeedsPermission | AttentionReason::NeedsInput, .. })
        );
        if self.pending.is_empty() {
            if blocked {
                self.attention = None;
            }
        } else if self.pending.iter().any(|p| !p.is_question) {
            self.raise(AttentionReason::NeedsPermission, now);
        } else {
            self.raise(AttentionReason::NeedsInput, now);
        }
    }
}

fn rank(r: AttentionReason) -> u8 {
    match r {
        AttentionReason::NeedsPermission | AttentionReason::NeedsInput => 3,
        AttentionReason::Errored => 2,
        AttentionReason::Finished => 1,
        AttentionReason::Unknown => 0,
    }
}

fn is_open(body: &ItemBody) -> bool {
    match body {
        ItemBody::Text { streaming, .. } | ItemBody::Thinking { streaming, .. } => *streaming,
        ItemBody::Tool { status, .. } => status.is_open(),
        _ => false,
    }
}

fn cap_output(mut s: String) -> String {
    if s.len() <= MAX_OUTPUT {
        return s;
    }
    let total = s.len();
    let mut cut = MAX_OUTPUT;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(&format!("\n… ({} more bytes not shown)", total - cut));
    s
}
