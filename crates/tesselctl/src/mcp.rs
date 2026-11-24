//! `tesselctl mcp`: tessel as tools for an agent, over the model context
//! protocol on stdin/stdout.
//!
//! every agent tessel starts gets this server, scoped to its own session by
//! `TESSEL_SESSION`. through it an agent can do anything a person can click
//! (the window's command table) and draw live ui in its chat (surfaces).
//! it's this small binary, not a runtime: it starts in milliseconds and only
//! connects to the daemon when a tool is first used.

use tessel_client::Client;
use tessel_proto::surface::{KINDS, SurfaceOp};
use tessel_proto::{Request, Response, SessionId};
use std::time::Duration;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WINDOW: &str = "Run a tessel window command: anything a person can click. Only when the person asks you to change the window. \
e.g. {\"cmd\":\"pane.show\",\"args\":{\"module\":\"changes\"}}, {\"cmd\":\"pane.size\",\"args\":{\"pane\":\"terminal\",\"size\":0.3}}. \
{\"cmd\":\"ui.ls\"} shows what's on screen; {\"cmd\":\"commands.list\"} lists every command.";

/// window commands an agent may not run. tessel's tools run without asking,
/// so anything that loses work or changes what agents may do stays with people.
const FOR_PEOPLE: &[(&str, &str)] = &[
    ("session.delete", "it deletes a session and its history"),
    ("daemon.set", "it changes what agents are allowed to do"),
    ("layout.reset", "it throws away the layout they made"),
];

fn tools() -> Value {
    let kinds: Vec<String> = KINDS.iter().map(|(k, about)| if about.is_empty() { (*k).to_owned() } else { format!("{k} ({about})") }).collect();
    let surface = format!(
        "Live UI in the chat, instead of markdown tables, ASCII art or status paragraphs, when a result reads better as UI \
(metrics, comparisons, checklists, progress, choices). ops: put (surface, node, region inline|pane), set (surface, element id or none for the top, props: merged in place), remove. \
An element is {{\"k\": kind, \"id\"?, \"p\": props, \"c\": children}}. Clicks and picks come back as <surface-event surface=… element=… act=… value=…/>. \
tessel draws it in its own style: you pick structure and tone, not colours. Kinds: {}.",
        kinds.join("; ")
    );
    json!([
        {
            "name": "tessel_window",
            "description": WINDOW,
            "inputSchema": {"type": "object", "properties": {
                "cmd": {"type": "string", "description": "a command id, like pane.split"},
                "args": {"type": "object", "description": "its arguments"}
            }, "required": ["cmd"]}
        },
        {
            "name": "tessel_layout",
            "description": "Read or replace the window layout (ui.json), only when asked. Read, change, write back; a write answers with what the checker found. Format: ui/README.md in tessel's home.",
            "inputSchema": {"type": "object", "properties": {
                "json": {"type": "object", "description": "the new ui.json; leave out to read the current one"}
            }}
        },
        {
            "name": "tessel_terminal",
            "description": "A terminal of the person's own, only when they ask for one. Run your own commands with Bash. \
ops: open (command: what to run, or leave out for a shell; show: put it in a new pane, only when the person asked for one, default false) -> its id; \
run (term, command: typed, then return); send (term, text: typed as is; \\u0003 is ctrl-c); \
read (term, history: also what scrolled away) -> its text; wait (term, text: until the screen shows it, or leave out to wait for it to exit; timeout seconds, default 30); \
list; close (term).",
            "inputSchema": {"type": "object", "properties": {
                "op": {"type": "string", "enum": ["open", "run", "send", "read", "wait", "list", "close"]},
                "term": {"type": "integer", "description": "which terminal (from open or list)"},
                "command": {"type": "string"},
                "text": {"type": "string"},
                "show": {"type": "boolean"},
                "history": {"type": "boolean"},
                "timeout": {"type": "number", "description": "wait: seconds"}
            }, "required": ["op"]}
        },
        {
            "name": "tessel_browser",
            "description": "A real browser page the person watches live. Use it to check UI you build: open (no url: this workspace's server), use it, screenshot it. \
snapshot gives an outline with refs (e12) to act on; take a new one after navigating. \
ops: open (url?), back, forward, reload, snapshot (root?), read (text, with selectors), screenshot, click (ref | selector | x,y), type (text, ref|selector?), select (value, ref|selector), key, scroll (dy, dx).",
            "inputSchema": {"type": "object", "properties": {
                "op": {"type": "string", "enum": ["open", "back", "forward", "reload", "snapshot", "read", "screenshot", "click", "type", "select", "key", "scroll"]},
                "url": {"type": "string"},
                "ref": {"type": "string", "description": "an element, as a snapshot names it: e12"},
                "root": {"type": "string", "description": "snapshot: only under this element (a ref or selector)"},
                "value": {"type": "string", "description": "select: the option's value or text"},
                "selector": {"type": "string", "description": "a CSS selector, as read gives them"},
                "x": {"type": "number"},
                "y": {"type": "number"},
                "text": {"type": "string"},
                "key": {"type": "string"},
                "dx": {"type": "number"},
                "dy": {"type": "number"}
            }, "required": ["op"]}
        },
        {
            "name": "tessel_workflow_save",
            "description": "Save a tessel workflow (YAML), so the person can run it again from a form. It answers with any problems and the format: fix them and save again until it saves clean.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string", "description": "the workflow's name (its file is named after it)"},
                "yaml": {"type": "string", "description": "the whole workflow, as YAML"}
            }, "required": ["name", "yaml"]}
        },
        {
            "name": "tessel_prove",
            "description": "Check your finished change: the repository's verify commands, its house rules (.tessel/rules) and independent reviewers. Answers with what failed and where.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "tessel_task",
            "description": "The board's tasks for this repository. ops: add (title, spec, after: ids it waits on, ready: start when a slot is free), list, note (id, text). Use it when asked to split work into tasks.",
            "inputSchema": {"type": "object", "properties": {
                "op": {"type": "string", "enum": ["add", "list", "note"]},
                "id": {"type": "string"},
                "title": {"type": "string"},
                "spec": {"type": "string"},
                "text": {"type": "string"},
                "after": {"type": "array", "items": {"type": "string"}},
                "ready": {"type": "boolean"}
            }, "required": ["op"]}
        },
        {
            "name": "tessel_ask",
            "description": "Ask the person and wait, as a small form in the chat, instead of asking in text and ending your turn. A few fields, the likely answer first. Answers come back as JSON (field id to answer; empty fields left out).",
            "inputSchema": {"type": "object", "properties": {
                "title": {"type": "string", "description": "the question, as a short heading"},
                "note": {"type": "string", "description": "a line of context, if it helps"},
                "fields": {"type": "array", "items": {"type": "object", "properties": {
                    "id": {"type": "string"},
                    "label": {"type": "string"},
                    "type": {"type": "string", "enum": ["text", "long", "select", "multiselect", "toggle"]},
                    "options": {"type": "array", "items": {"type": "string"}},
                    "optional": {"type": "boolean"},
                    "placeholder": {"type": "string"}
                }, "required": ["id", "label"]}},
                "submit": {"type": "string", "description": "the button's label (default Continue)"}
            }, "required": ["fields"]}
        },
        {
            "name": "tessel_surface",
            "description": surface,
            "inputSchema": {"type": "object", "properties": {
                "op": {"type": "string", "enum": ["put", "set", "remove"]},
                "surface": {"type": "string", "description": "a name for the surface, yours to choose"},
                "node": {"type": "object", "description": "put: the element tree"},
                "region": {"type": "string", "enum": ["inline", "pane"], "description": "put: where it shows (default inline)"},
                "element": {"type": "string", "description": "set: the id of the element to change; leave out for the surface's top element"},
                "props": {"type": "object", "description": "set: props to merge (null removes one)"}
            }, "required": ["op", "surface"]}
        }
    ])
}

struct Server {
    client: Option<Client>,
    session: Option<SessionId>,
    /// a screenshot the last tool took, sent along with its words.
    image: Option<String>,
}

impl Server {
    async fn client(&mut self) -> anyhow::Result<&Client> {
        // the daemon went (a restart hands over to a new one): connect to the one there is now.
        if self.client.as_ref().is_some_and(Client::is_closed) {
            self.client = None;
        }
        if self.client.is_none() {
            let home = tessel_proto::local::home();
            let (c, _, _) = tessel_client::connect_or_start(&home, "tesselctl-mcp").await?;
            self.client = Some(c);
        }
        Ok(self.client.as_ref().expect("just set"))
    }

    /// ask the daemon. a connection that went (the daemon restarted) is made
    /// again and the question asked once more; nothing waits for ever.
    async fn ask(&mut self, req: Request) -> anyhow::Result<Response> {
        let first = self.client().await?.request(req.clone()).await;
        match first {
            Err(tessel_client::ClientError::Closed) => {
                self.client = None;
                Ok(self.client().await?.request(req).await?)
            }
            other => Ok(other?),
        }
    }

    async fn window(&mut self, cmd: &str, args: Value) -> anyhow::Result<Value> {
        if let Some((_, why)) = FOR_PEOPLE.iter().find(|(c, _)| *c == cmd) {
            anyhow::bail!("{cmd} is for the person to do, not an agent: {why}. ask them if it's needed");
        }
        // a window that doesn't answer is said to, rather than waited on for ever.
        let reply = within(Duration::from_secs(30), "the window", self.ask(Request::UiExec { window: None, cmd: cmd.to_owned(), args })).await??;
        match reply {
            Response::UiResult { value } => Ok(value),
            other => anyhow::bail!("unexpected reply: {other:?}"),
        }
    }

    async fn terminal(&mut self, args: &Value) -> anyhow::Result<String> {
        let op = args.get("op").and_then(Value::as_str).unwrap_or("");
        let term = || args.get("term").and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("{op} needs \"term\" (open or list gives one)"));
        let text_of = |r: Response| match r {
            Response::TermText { text } => Ok(text),
            other => Err(anyhow::anyhow!("unexpected reply: {other:?}")),
        };
        Ok(match op {
            // a command, run where the person sees it: its own terminal, which the
            // window shows in the session's terminal pane (visual mode follows it).
            "open" => {
                let command: Vec<String> = args.get("command").and_then(Value::as_str).filter(|c| !c.trim().is_empty()).map(|c| vec![c.to_owned()]).unwrap_or_default();
                let session = self.session;
                let reply = self.ask(Request::TermOpen { session, cwd: None, command, cols: 120, rows: 32, agent: false }).await?;
                let Response::Term { info } = reply else { anyhow::bail!("unexpected reply") };
                // a pane for it: a new terminal pane shows a running one of this session that no pane shows.
                let shown = if args.get("show").and_then(Value::as_bool).unwrap_or(false) {
                    match self.window("terminal.new", json!({})).await {
                        Ok(_) => " and shown in a pane".to_owned(),
                        Err(e) => format!(" (not shown: {e})"),
                    }
                } else {
                    String::new()
                };
                format!("terminal {} is running in {}{shown}", info.id, info.cwd)
            }
            "run" | "send" => {
                let mut text = args.get(if op == "run" { "command" } else { "text" }).and_then(Value::as_str).unwrap_or("").to_owned();
                if op == "run" {
                    text.push('\r');
                }
                let term = term()?;
                self.ask(Request::TermInput { term, text }).await?;
                "typed".to_owned()
            }
            "read" => {
                let (term, history) = (term()?, args.get("history").and_then(Value::as_bool).unwrap_or(false));
                text_of(self.ask(Request::TermCapture { term, history, format: None }).await?)?
            }
            "wait" => {
                let term = term()?;
                let text = args.get("text").and_then(Value::as_str).map(str::to_owned);
                let timeout_ms = (args.get("timeout").and_then(Value::as_f64).unwrap_or(30.0).clamp(0.1, 600.0) * 1000.0) as u64;
                match self.ask(Request::TermWait { term, text, timeout_ms }).await? {
                    Response::TermText { text } => format!("the screen now:\n{text}"),
                    Response::Term { info } => format!("it exited with {}", info.exited.unwrap_or(-1)),
                    other => anyhow::bail!("unexpected reply: {other:?}"),
                }
            }
            "list" => {
                let Response::Terms { terms } = self.ask(Request::TermList).await? else { anyhow::bail!("unexpected reply") };
                let mine: Vec<String> = terms
                    .iter()
                    .filter(|t| self.session.is_none() || t.session == self.session)
                    .map(|t| format!("{}: {} in {}{}", t.id, if t.command.is_empty() { "shell".to_owned() } else { t.command.join(" ") }, t.cwd, t.exited.map(|c| format!(" (exited {c})")).unwrap_or_default()))
                    .collect();
                if mine.is_empty() { "no terminals".to_owned() } else { mine.join("\n") }
            }
            "close" => {
                let term = term()?;
                self.ask(Request::TermClose { term }).await?;
                "closed".to_owned()
            }
            other => anyhow::bail!("tessel_terminal has no op {other:?}: open, run, send, read, wait, list or close"),
        })
    }

    /// the browser the person watches: open, look, click, type.
    async fn browser(&mut self, args: &Value) -> anyhow::Result<String> {
        use tessel_proto::BrowserAct;
        let session = self.session.ok_or_else(|| anyhow::anyhow!("the browser belongs to a session, and this agent wasn't started by tessel"))?;
        let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_owned);
        let n = |k: &str| args.get(k).and_then(Value::as_f64);
        let op = s("op").ok_or_else(|| anyhow::anyhow!("tessel_browser needs \"op\""))?;
        let act = match op.as_str() {
            "open" => BrowserAct::Open { url: s("url") },
            "back" => BrowserAct::Back,
            "forward" => BrowserAct::Forward,
            "reload" => BrowserAct::Reload,
            "read" => BrowserAct::Read,
            "snapshot" => BrowserAct::Snapshot { root: s("root") },
            "select" => BrowserAct::Select { value: s("value").ok_or_else(|| anyhow::anyhow!("select needs \"value\""))?, r#ref: s("ref"), selector: s("selector") },
            "screenshot" => BrowserAct::Screenshot,
            "click" => BrowserAct::Click { r#ref: s("ref"), selector: s("selector"), x: n("x"), y: n("y") },
            "type" => BrowserAct::Type { text: s("text").ok_or_else(|| anyhow::anyhow!("type needs \"text\""))?, r#ref: s("ref"), selector: s("selector") },
            "key" => BrowserAct::Key { key: s("key").ok_or_else(|| anyhow::anyhow!("key needs \"key\": Enter, Tab, Escape…"))? },
            "scroll" => BrowserAct::Scroll { dx: n("dx").unwrap_or(0.0), dy: n("dy").unwrap_or(400.0), x: n("x"), y: n("y") },
            other => anyhow::bail!("tessel_browser has no op {other:?}: open, back, forward, reload, snapshot, read, screenshot, click, type, select, key or scroll"),
        };
        let reply = within(Duration::from_secs(90), "the browser", self.ask(Request::Browser { session, act, agent: true })).await??;
        let Response::Browser { state, text, image } = reply else { anyhow::bail!("unexpected reply: {reply:?}") };
        let at = format!("{} ({})", if state.title.is_empty() { "untitled" } else { &state.title }, state.url);
        Ok(match (text, image) {
            (Some(text), _) => text,
            (None, Some(png)) => {
                self.image = Some(png);
                format!("the page now: {at}")
            }
            (None, None) => format!("done. the page: {at}"),
        })
    }

    /// run one tool. the text is what the agent reads back.
    async fn call(&mut self, name: &str, args: &Value) -> anyhow::Result<String> {
        let text = |v: Value| if v.is_null() { "done".to_owned() } else { serde_json::to_string_pretty(&v).unwrap_or_default() };
        Ok(match name {
            "tessel_window" => {
                let cmd = args.get("cmd").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("tessel_window needs \"cmd\""))?;
                text(self.window(cmd, args.get("args").cloned().unwrap_or(json!({}))).await?)
            }
            "tessel_window_state" => text(self.window("ui.ls", json!({})).await?),
            "tessel_commands" => {
                let all = self.window("commands.list", json!({})).await?;
                let mut out = String::new();
                let mut seen = std::collections::HashSet::new();
                for c in all.as_array().into_iter().flatten() {
                    let id = c["id"].as_str().unwrap_or("");
                    if seen.insert(id.to_owned()) {
                        out.push_str(&format!("{id}: {}{}\n", c["title"].as_str().unwrap_or(""), c["args"].as_str().filter(|a| !a.is_empty()).map(|a| format!(": {a}")).unwrap_or_default()));
                    }
                }
                out
            }
            "tessel_layout" => match args.get("json") {
                Some(json) => text(self.window("layout.set", json!({"json": json})).await?),
                None => text(self.window("layout.get", json!({})).await?),
            },
            "tessel_surface" => {
                let session = self.session.ok_or_else(|| anyhow::anyhow!("surfaces belong to a session, and this agent wasn't started by tessel"))?;
                let mut op = args.clone();
                if let Some(o) = op.as_object_mut() {
                    o.retain(|_, v| !v.is_null());
                }
                let op: SurfaceOp = serde_json::from_value(op).map_err(|e| anyhow::anyhow!("that isn't a surface op: {e}"))?;
                self.ask(Request::SurfaceOp { session, op }).await?;
                "done: it's on screen".to_owned()
            }
            "tessel_workflow_save" => {
                let name = args.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                let yaml = args.get("yaml").and_then(Value::as_str).unwrap_or("").to_owned();
                match self.ask(Request::WorkflowSave { name, yaml, cwd: None }).await {
                    Ok(Response::WorkflowSaved { workflow }) if workflow.problems.is_empty() => format!("saved {} at {}. it's in the person's workflows now", workflow.name, workflow.path),
                    Ok(Response::WorkflowSaved { workflow }) => format!("saved at {}, but it has problems to fix, then save again:\n- {}\n\n{}", workflow.path, workflow.problems.join("\n- "), tessel_proto::workflow::FORMAT),
                    Ok(other) => anyhow::bail!("unexpected reply: {other:?}"),
                    Err(e) => anyhow::bail!("{e:#}\n\n{}", tessel_proto::workflow::FORMAT),
                }
            }
            "tessel_task" => {
                let s = |k: &str| args.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                let line = |t: &tessel_proto::task::Task| format!("{} {:?} {}{}", t.id, t.state, t.title, if t.deps.is_empty() { String::new() } else { format!(" (after {})", t.deps.join(", ")) });
                match s("op").as_str() {
                    "add" => {
                        let repo = std::env::current_dir()?.to_string_lossy().into_owned();
                        let deps = args.get("after").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
                        let ready = args.get("ready").and_then(Value::as_bool).unwrap_or(false);
                        match self.ask(Request::TaskCreate { repo, title: s("title"), spec: s("spec"), deps, parent: None, ready, budget_usd: None, plan: false, from: self.session, attachments: Vec::new() }).await? {
                            Response::Task { task } => format!("added {}", line(&task)),
                            other => anyhow::bail!("unexpected reply: {other:?}"),
                        }
                    }
                    "note" => match self.ask(Request::TaskEdit { id: s("id"), title: None, spec: None, deps: None, note: Some(s("text")) }).await? {
                        Response::Task { task } => format!("noted on {}", line(&task)),
                        other => anyhow::bail!("unexpected reply: {other:?}"),
                    },
                    _ => match self.ask(Request::TaskList).await? {
                        Response::Tasks { tasks } => {
                            let here = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
                            let mine: Vec<String> = tasks.iter().filter(|t| here.starts_with(&t.repo) || t.repo.starts_with(&here)).map(line).collect();
                            if mine.is_empty() { "no tasks here".to_owned() } else { mine.join("\n") }
                        }
                        other => anyhow::bail!("unexpected reply: {other:?}"),
                    },
                }
            }
            "tessel_prove" => {
                let session = self.session.ok_or_else(|| anyhow::anyhow!("proving belongs to a session, and this agent wasn't started by tessel"))?;
                let current = |r: Response| match r {
                    Response::Sessions { sessions } => sessions.into_iter().find(|s| s.id == session).and_then(|s| s.proof),
                    _ => None,
                };
                let before = current(self.ask(Request::SessionList).await?).map(|p| p.started_ms);
                self.ask(Request::ProofRun { session, anyway: false }).await?;
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let Some(p) = current(self.ask(Request::SessionList).await?) else { continue };
                    if Some(p.started_ms) != before && p.verdict.done() {
                        break p.report();
                    }
                }
            }
            "tessel_ask" => {
                let session = self.session.ok_or_else(|| anyhow::anyhow!("asking belongs to a session, and this agent wasn't started by tessel"))?;
                match self.ask(Request::Ask { session, form: args.clone() }).await? {
                    Response::Answered { answers } => serde_json::to_string(&answers)?,
                    other => anyhow::bail!("unexpected reply: {other:?}"),
                }
            }
            "tessel_terminal" => self.terminal(args).await?,
            "tessel_browser" => self.browser(args).await?,
            other => anyhow::bail!("there's no tool {other}"),
        })
    }
}

/// serve until stdin closes.
pub async fn serve() -> anyhow::Result<()> {
    let session = std::env::var("TESSEL_SESSION").ok().and_then(|s| s.parse().ok());
    let mut server = Server { client: None, session, image: None };
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    let mut out = tokio::io::stdout();
    while let Some(line) = lines.next_line().await? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        // notifications have no id and want no answer.
        let Some(id) = msg.get("id").cloned() else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "tessel", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => {
                let ruled = std::env::current_dir().is_ok_and(|d| tessel_proto::proof::has_house_rules(&d));
                let all = tools();
                let listed: Vec<&Value> = all.as_array().into_iter().flatten().filter(|t| ruled || t["name"] != "tessel_prove").collect();
                Ok(json!({"tools": listed}))
            }
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                // a backstop: one call never holds up every call after it (they're answered in turn).
                // waiting on a terminal may take as long as it was asked to.
                let asked = args.get("timeout").and_then(Value::as_f64).unwrap_or(0.0).clamp(0.0, 3600.0);
                let limit = if name == "tessel_prove" { Duration::from_secs(45 * 60) } else if name == "tessel_ask" { Duration::from_secs(24 * 3600) } else { Duration::from_secs_f64(150f64.max(asked + 30.0)) };
                let done = match tokio::time::timeout(limit, server.call(&name, &args)).await {
                    Ok(done) => done,
                    Err(_) => {
                        // the connection it was waiting on may be the trouble: the next call makes a new one.
                        server.client = None;
                        Err(anyhow::anyhow!("{name} didn't finish in {}s, so it was given up on. tessel may need restarting", limit.as_secs()))
                    }
                };
                Ok(match done {
                    Ok(text) => {
                        let mut content = vec![json!({"type": "text", "text": text})];
                        if let Some(png) = server.image.take() {
                            content.push(json!({"type": "image", "data": png, "mimeType": "image/png"}));
                        }
                        json!({"content": content, "isError": false})
                    }
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                })
            }
            other => Err(json!({"code": -32601, "message": format!("tessel doesn't do {other}")})),
        };
        let reply = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        out.write_all(format!("{reply}\n").as_bytes()).await?;
        out.flush().await?;
    }
    Ok(())
}

/// `fut`, or after `limit` an error saying what didn't answer: a tool call must
/// end, so the agent can carry on (a frozen page, an older daemon).
async fn within<T>(limit: Duration, what: &str, fut: impl std::future::Future<Output = T>) -> anyhow::Result<T> {
    tokio::time::timeout(limit, fut).await.map_err(|_| anyhow::anyhow!("{what} didn't answer in {}s. the page may be frozen (a script that never yields), or tessel may need restarting", limit.as_secs()))
}
