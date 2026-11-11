//! claude's stdout, one json line at a time, into `AgentEvent`s.
//!
//! pure and synchronous: `(state, line) -> events`. no io, no clock. that's
//! what lets the same code run live, in golden tests, and when rebuilding a
//! session from its raw log.
//!
//! tolerant on purpose. a line we can't parse or don't recognise yields no
//! events and never an error: a new message type in a future cli must not
//! take a session down.

use tessel_proto::{
    AgentEvent, AgentPhase, BlockKind, ItemId, PermissionRequest, RequestId, TurnOutcome,
};
use serde_json::Value;
use std::collections::HashMap;

#[derive(Default)]
pub struct ClaudeNormalizer {
    /// blocks currently streaming, keyed by (api message id, block index).
    open: Vec<OpenBlock>,
    /// how many completed blocks we've seen per api message id. gives ids to
    /// blocks that never streamed (subagent messages arrive whole).
    completed: HashMap<String, u64>,
    /// the model that wrote the latest main-thread reply. tells us which
    /// model's window to report when a turn ends.
    model: Option<String>,
    /// the last context size we reported, so we only say so when it changes.
    context: Option<u64>,
}

struct OpenBlock {
    message: String,
    index: u64,
    kind: Kind,
    item: ItemId,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Text,
    Thinking,
    Tool,
}

impl ClaudeNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(&mut self, line: &str) -> Vec<AgentEvent> {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        self.push_value(&v)
    }

    pub fn push_value(&mut self, v: &Value) -> Vec<AgentEvent> {
        match str_at(v, "type") {
            Some("system") => self.system(v),
            Some("stream_event") => self.stream_event(v),
            Some("assistant") => self.assistant(v),
            Some("user") => self.user(v),
            Some("result") => self.result(v),
            Some("control_request") => self.control_request(v),
            Some("control_cancel_request") => match str_at(v, "request_id") {
                Some(id) => vec![AgentEvent::PermissionCancelled { request_id: RequestId(id.to_owned()) }],
                None => Vec::new(),
            },
            Some("rate_limit_event") => self.rate_limit(v),
            // answers to requests *we* made: the model or mode we asked for is now in effect.
            Some("control_response") => {
                let answer = v.get("response").filter(|r| str_at(r, "subtype") == Some("success")).and_then(|r| r.get("response"));
                let mut out = Vec::new();
                if let Some(r) = v.get("response").filter(|r| str_at(r, "subtype") == Some("error")) {
                    out.push(AgentEvent::RequestFailed { message: str_at(r, "error").unwrap_or("the agent refused a request").to_owned() });
                }
                if let Some(model) = answer.and_then(|a| str_at(a, "model")) {
                    out.push(AgentEvent::ModelChanged { model: model.to_owned() });
                }
                if let Some(servers) = answer.and_then(|a| a.get("mcpServers")) {
                    out.push(AgentEvent::McpServers { servers: mcp_servers(Some(servers)) });
                }
                if let Some(mode) = answer.and_then(|a| str_at(a, "mode")) {
                    out.push(AgentEvent::ModeChanged { mode: mode.to_owned() });
                }
                out
            }
            _ => Vec::new(),
        }
    }

    fn system(&mut self, v: &Value) -> Vec<AgentEvent> {
        match str_at(v, "subtype") {
            Some("init") => vec![AgentEvent::Init {
                agent_session_id: str_at(v, "session_id").unwrap_or_default().to_owned(),
                model: owned(v, "model"),
                cwd: owned(v, "cwd"),
                permission_mode: owned(v, "permissionMode"),
                agent_version: owned(v, "claude_code_version"),
                mcp_servers: mcp_servers(v.get("mcp_servers")),
            }],
            Some("session_state_changed") => {
                let phase = match str_at(v, "state") {
                    Some("running") => AgentPhase::Running,
                    Some("requires_action") => AgentPhase::RequiresAction,
                    Some("idle") => AgentPhase::Idle,
                    _ => AgentPhase::Unknown,
                };
                vec![AgentEvent::Phase { phase }]
            }
            Some("task_started") => match str_at(v, "task_id") {
                Some(id) => vec![AgentEvent::TaskStarted {
                    task_id: id.to_owned(),
                    tool_item: str_at(v, "tool_use_id").map(ItemId::from),
                    description: str_at(v, "description").unwrap_or_default().to_owned(),
                    background: v.get("is_backgrounded").and_then(Value::as_bool).unwrap_or(false),
                }],
                None => Vec::new(),
            },
            Some("task_progress") => match str_at(v, "task_id") {
                Some(id) => vec![AgentEvent::TaskProgress {
                    task_id: id.to_owned(),
                    description: str_at(v, "description").unwrap_or_default().to_owned(),
                }],
                None => Vec::new(),
            },
            Some("task_notification") => match str_at(v, "task_id") {
                Some(id) => vec![AgentEvent::TaskFinished {
                    task_id: id.to_owned(),
                    status: str_at(v, "status").unwrap_or("completed").to_owned(),
                    summary: owned(v, "summary"),
                }],
                None => Vec::new(),
            },
            Some("task_updated") => {
                // a patch only ends a task when it carries a terminal status.
                let status = v.get("patch").and_then(|p| str_at(p, "status"));
                match (str_at(v, "task_id"), status) {
                    (Some(id), Some(s)) if matches!(s, "completed" | "failed" | "killed" | "cancelled" | "stopped") => {
                        vec![AgentEvent::TaskFinished { task_id: id.to_owned(), status: s.to_owned(), summary: None }]
                    }
                    _ => Vec::new(),
                }
            }
            // the agent reports a mode change here too: approving a plan
            // drops it out of plan mode by itself.
            Some("status") => match str_at(v, "permissionMode") {
                Some(mode) => vec![AgentEvent::ModeChanged { mode: mode.to_owned() }],
                None => Vec::new(),
            },
            // hook_started, hook_response, thinking_tokens, ui_log,
            // commands_changed, background_tasks_changed: nothing we act on yet.
            _ => Vec::new(),
        }
    }

    fn stream_event(&mut self, v: &Value) -> Vec<AgentEvent> {
        let Some(ev) = v.get("event") else { return Vec::new() };
        let message = str_at(v, "api_message_id").unwrap_or_default();
        let parent = parent_of(v);
        let index = ev.get("index").and_then(Value::as_u64).unwrap_or(0);
        match str_at(ev, "type") {
            Some("content_block_start") => {
                let Some(block) = ev.get("content_block") else { return Vec::new() };
                let (kind, item, block_kind) = match str_at(block, "type") {
                    Some("text") => (Kind::Text, block_id(message, index), BlockKind::Text),
                    Some("thinking") => (Kind::Thinking, block_id(message, index), BlockKind::Thinking),
                    Some("tool_use") => {
                        let Some(id) = str_at(block, "id") else { return Vec::new() };
                        let tool = str_at(block, "name").unwrap_or_default().to_owned();
                        (Kind::Tool, ItemId::from(id), BlockKind::Tool { tool })
                    }
                    _ => return Vec::new(),
                };
                self.open.push(OpenBlock { message: message.to_owned(), index, kind, item: item.clone() });
                vec![AgentEvent::BlockStarted { item_id: item, parent, block: block_kind }]
            }
            Some("content_block_delta") => {
                let Some(delta) = ev.get("delta") else { return Vec::new() };
                let text = match str_at(delta, "type") {
                    Some("text_delta") => str_at(delta, "text"),
                    Some("thinking_delta") => str_at(delta, "thinking"),
                    // tool input arrives whole in the `assistant` line.
                    _ => None,
                };
                match (text, self.open.iter().find(|b| b.message == message && b.index == index)) {
                    (Some(t), Some(b)) if !t.is_empty() => {
                        vec![AgentEvent::Delta { item_id: b.item.clone(), text: t.to_owned() }]
                    }
                    _ => Vec::new(),
                }
            }
            Some("message_stop") => {
                // anything still open for this message will never complete.
                self.open.retain(|b| b.message != message);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn assistant(&mut self, v: &Value) -> Vec<AgentEvent> {
        let Some(msg) = v.get("message") else { return Vec::new() };
        let message = str_at(msg, "id").unwrap_or_default().to_owned();
        let parent = parent_of(v);
        let mut out = Vec::new();
        // the size of the conversation is what the model was just sent: fresh
        // tokens plus everything served from or written to the cache. only the
        // main thread counts; a subagent has a context of its own.
        if parent.is_none() {
            if let Some(model) = str_at(msg, "model") {
                self.model = Some(model.to_owned());
            }
            if let Some(usage) = msg.get("usage") {
                let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
                let tokens = n("input_tokens") + n("cache_read_input_tokens") + n("cache_creation_input_tokens");
                if tokens > 0 && self.context != Some(tokens) {
                    self.context = Some(tokens);
                    out.push(AgentEvent::ContextUsed { tokens });
                }
            }
        }
        for block in msg.get("content").and_then(Value::as_array).into_iter().flatten() {
            match str_at(block, "type") {
                Some("text") => {
                    let item_id = self.complete(&message, Kind::Text, None);
                    let text = str_at(block, "text").unwrap_or_default().to_owned();
                    out.push(AgentEvent::Text { item_id, parent: parent.clone(), text });
                }
                Some("thinking") => {
                    let item_id = self.complete(&message, Kind::Thinking, None);
                    let text = str_at(block, "thinking").unwrap_or_default().to_owned();
                    out.push(AgentEvent::Thinking { item_id, parent: parent.clone(), text });
                }
                Some("tool_use") => {
                    let Some(id) = str_at(block, "id") else { continue };
                    let item_id = self.complete(&message, Kind::Tool, Some(id));
                    out.push(AgentEvent::ToolCall {
                        item_id,
                        parent: parent.clone(),
                        tool: str_at(block, "name").unwrap_or_default().to_owned(),
                        input: block.get("input").cloned().unwrap_or(Value::Null),
                    });
                }
                _ => {
                    // keep the per-message count honest for kinds we skip.
                    *self.completed.entry(message.clone()).or_default() += 1;
                }
            }
        }
        out
    }

    /// pick the id for a completed block: the streaming block it finishes if
    /// there is one, otherwise the next slot for that message.
    fn complete(&mut self, message: &str, kind: Kind, tool_id: Option<&str>) -> ItemId {
        let n = self.completed.entry(message.to_owned()).or_default();
        let slot = *n;
        *n += 1;
        let found = self.open.iter().position(|b| {
            b.message == message
                && b.kind == kind
                && tool_id.is_none_or(|id| b.item.as_str() == id)
        });
        match (found, tool_id) {
            (Some(i), _) => self.open.remove(i).item,
            (None, Some(id)) => ItemId::from(id),
            (None, None) => block_id(message, slot),
        }
    }

    fn user(&mut self, v: &Value) -> Vec<AgentEvent> {
        let Some(msg) = v.get("message") else { return Vec::new() };
        let replay = v.get("isReplay").and_then(Value::as_bool).unwrap_or(false);
        match msg.get("content") {
            // the cli reporting on something it did itself (switching model,
            // say) in the shape of a message from the user. nobody typed it,
            // and treating it as one would start a turn that never ends.
            Some(Value::String(text)) if text.trim_start().starts_with("<local-command-") => Vec::new(),
            // our own message, echoed back once the agent accepted it.
            Some(Value::String(text)) if replay => {
                let id = str_at(v, "uuid").map(ItemId::from).unwrap_or_else(|| ItemId::from("user"));
                vec![AgentEvent::UserMessage { item_id: id, text: text.clone() }]
            }
            // the same, when it was sent with attachments: the text is one block among several.
            Some(Value::Array(blocks)) if replay => {
                let id = str_at(v, "uuid").map(ItemId::from).unwrap_or_else(|| ItemId::from("user"));
                let text = blocks.iter().filter(|b| str_at(b, "type") == Some("text")).filter_map(|b| str_at(b, "text")).collect::<Vec<_>>().join("\n");
                vec![AgentEvent::UserMessage { item_id: id, text }]
            }
            Some(Value::Array(blocks)) => blocks
                .iter()
                .filter(|b| str_at(b, "type") == Some("tool_result"))
                .filter_map(|b| {
                    let id = str_at(b, "tool_use_id")?;
                    Some(AgentEvent::ToolResult {
                        item_id: ItemId::from(id),
                        ok: !b.get("is_error").and_then(Value::as_bool).unwrap_or(false),
                        output: content_text(b.get("content")),
                    })
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    fn result(&mut self, v: &Value) -> Vec<AgentEvent> {
        // a turn is over: nothing from it can still be streaming.
        self.open.clear();
        let terminal = str_at(v, "terminal_reason").unwrap_or_default();
        let is_error = v.get("is_error").and_then(Value::as_bool).unwrap_or(false);
        let outcome = if terminal.starts_with("aborted") {
            TurnOutcome::Interrupted
        } else if !is_error && str_at(v, "subtype") == Some("success") {
            TurnOutcome::Completed
        } else {
            let errors: Vec<&str> = v
                .get("errors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            let message = if !errors.is_empty() {
                errors.join("; ")
            } else if let Some(r) = str_at(v, "result").filter(|r| !r.is_empty()) {
                r.to_owned()
            } else {
                str_at(v, "subtype").unwrap_or("error").to_owned()
            };
            TurnOutcome::Error { message }
        };
        let mut out = Vec::new();
        // how much the model that is doing the work can hold.
        let usage = v.get("modelUsage").and_then(Value::as_object);
        let window = usage.and_then(|u| {
            let mine = self.model.as_deref().and_then(|m| u.get(m));
            mine.or_else(|| u.values().next()).and_then(|m| m.get("contextWindow")).and_then(Value::as_u64)
        });
        if let Some(tokens) = window {
            out.push(AgentEvent::ContextWindow { tokens });
        }
        out.push(AgentEvent::TurnFinished {
            outcome,
            cost_usd: v.get("total_cost_usd").and_then(Value::as_f64),
            duration_ms: v.get("duration_ms").and_then(Value::as_u64),
        });
        out
    }

    fn control_request(&mut self, v: &Value) -> Vec<AgentEvent> {
        let (Some(id), Some(req)) = (str_at(v, "request_id"), v.get("request")) else {
            return Vec::new();
        };
        if str_at(req, "subtype") != Some("can_use_tool") {
            return Vec::new();
        }
        let tool = str_at(req, "tool_name").unwrap_or_default().to_owned();
        // leaving plan mode is neither a permission nor a question: it is a
        // plan waiting to be approved.
        let plan = (tool == "ExitPlanMode").then(|| req.get("input").and_then(|i| str_at(i, "plan")).unwrap_or_default().to_owned());
        let is_question = plan.is_none()
            && (req.get("requires_user_interaction").and_then(Value::as_bool).unwrap_or(false) || tool == "AskUserQuestion");
        vec![AgentEvent::PermissionRequested {
            request: PermissionRequest {
                request_id: RequestId(id.to_owned()),
                tool_item: str_at(req, "tool_use_id").map(ItemId::from),
                tool,
                input: req.get("input").cloned().unwrap_or(Value::Null),
                description: owned(req, "description"),
                is_question,
                plan,
                suggestions: req
                    .get("permission_suggestions")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            },
        }]
    }

    fn rate_limit(&mut self, v: &Value) -> Vec<AgentEvent> {
        let Some(info) = v.get("rate_limit_info") else { return Vec::new() };
        let utilization = info
            .get("unifiedWindows")
            .and_then(Value::as_object)
            .into_iter()
            .flat_map(|w| w.values())
            .filter_map(|w| w.get("utilization").and_then(Value::as_f64))
            .fold(None, |max: Option<f64>, u| Some(max.map_or(u, |m| m.max(u))));
        vec![AgentEvent::RateLimit {
            status: str_at(info, "status").unwrap_or("unknown").to_owned(),
            resets_at: info.get("resetsAt").and_then(Value::as_i64),
            utilization,
        }]
    }
}

/// if this is a control request we do not handle, return its id so the caller
/// can refuse it instead of leaving the cli waiting.
pub fn unhandled_control_request(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line).ok()?;
    if str_at(&v, "type") != Some("control_request") {
        return None;
    }
    let subtype = v.get("request").and_then(|r| str_at(r, "subtype"));
    if subtype == Some("can_use_tool") {
        return None;
    }
    str_at(&v, "request_id").map(str::to_owned)
}

fn block_id(message: &str, index: u64) -> ItemId {
    ItemId(format!("{message}/{index}"))
}

fn parent_of(v: &Value) -> Option<ItemId> {
    str_at(v, "parent_tool_use_id").map(ItemId::from)
}

fn str_at<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

fn owned(v: &Value, key: &str) -> Option<String> {
    str_at(v, key).map(str::to_owned)
}

/// a tool result's content is either a string or a list of blocks.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter_map(|b| str_at(b, "text"))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn mcp_servers(v: Option<&Value>) -> Vec<tessel_proto::McpServer> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|s| {
            Some(tessel_proto::McpServer {
                name: str_at(s, "name")?.to_owned(),
                status: str_at(s, "status").unwrap_or("unknown").to_owned(),
                scope: str_at(s, "scope").map(str::to_owned),
            })
        })
        .collect()
}
