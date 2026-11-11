//! `photonctl mcp`: photon as tools for an agent, over the model context
//! protocol on stdin/stdout.
//!
//! every agent photon starts gets this server, scoped to its own session by
//! `PHOTON_SESSION`. through it an agent can do anything a person can click
//! (the window's command table) and draw live ui in its chat (surfaces).
//! it's this small binary, not a runtime: it starts in milliseconds and only
//! connects to the daemon when a tool is first used.

use photon_client::Client;
use photon_proto::surface::{KINDS, SurfaceOp};
use photon_proto::{Request, Response, SessionId};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

const WINDOW: &str = "Run any command in the photon window: everything a person can click. \
Examples: {\"cmd\":\"pane.split\",\"args\":{\"dir\":\"right\"}}, {\"cmd\":\"pane.show\",\"args\":{\"module\":\"changes\"}}, \
{\"cmd\":\"theme.set\",\"args\":{\"base\":\"tern\"}}, {\"cmd\":\"session.open\",\"args\":{\"session\":\"<id prefix>\"}}. \
Panes are named by id, @focused, or the module they show. photon_window_state shows what is on screen; photon_commands lists every command.";

/// window commands an agent may not run. photon's tools run without asking,
/// so anything that loses work or changes what agents may do stays with people.
const FOR_PEOPLE: &[(&str, &str)] = &[
    ("session.delete", "it deletes a session and its history"),
    ("daemon.set", "it changes what agents are allowed to do"),
    ("layout.reset", "it throws away the layout they made"),
];

fn tools() -> Value {
    let kinds: Vec<String> = KINDS.iter().map(|(k, about)| if about.is_empty() { (*k).to_owned() } else { format!("{k} ({about})") }).collect();
    let surface = format!(
        "Draw live UI in your chat (or in a Surfaces pane) and update it in place: progress, status cards, \
choices for the person, results. ops: put (draw or redraw a whole surface: surface, node, region inline|pane; \
pane surfaces show in a Surfaces pane, opened with photon_window pane.show {{\"module\":\"surfaces\"}}, and in the chat while none is open), \
set (change one element's props in place: surface, element (an id; leave out for the top element), props), remove (surface). An element is \
{{\"k\": kind, \"id\": optional, \"p\": props, \"c\": children}}. When the person clicks a button, ticks a checkbox \
or picks a choice you get a message <surface-event surface=… element=… act=… value=…/>. Everything is drawn in photon's own \
style; you choose structure and tone, not colours. Kinds: {}.",
        kinds.join("; ")
    );
    json!([
        {
            "name": "photon_window",
            "description": WINDOW,
            "inputSchema": {"type": "object", "properties": {
                "cmd": {"type": "string", "description": "a command id, like pane.split"},
                "args": {"type": "object", "description": "its arguments"}
            }, "required": ["cmd"]}
        },
        {
            "name": "photon_window_state",
            "description": "What the photon window shows right now: the open session, panes (id, module, focused, size), drawers, theme.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "photon_commands",
            "description": "Every command photon_window takes, with its arguments and shortcut.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "photon_layout",
            "description": "Read or replace the whole window layout (ui.json). Read first, change what you need, write it back. \
A write answers with what photon's checker found (errors are refused, warnings and adjustments listed). \
The reference for the format is ui/README.md in photon's home.",
            "inputSchema": {"type": "object", "properties": {
                "json": {"type": "object", "description": "the new ui.json; leave out to read the current one"}
            }}
        },
        {
            "name": "photon_surface",
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
}

impl Server {
    async fn client(&mut self) -> anyhow::Result<&Client> {
        if self.client.is_none() {
            let home = photon_proto::local::home();
            let (c, _, _) = photon_client::connect_or_start(&home, "photonctl-mcp").await?;
            self.client = Some(c);
        }
        Ok(self.client.as_ref().expect("just set"))
    }

    async fn window(&mut self, cmd: &str, args: Value) -> anyhow::Result<Value> {
        if let Some((_, why)) = FOR_PEOPLE.iter().find(|(c, _)| *c == cmd) {
            anyhow::bail!("{cmd} is for the person to do, not an agent: {why}. ask them if it's needed");
        }
        let reply = self.client().await?.request(Request::UiExec { window: None, cmd: cmd.to_owned(), args }).await?;
        match reply {
            Response::UiResult { value } => Ok(value),
            other => anyhow::bail!("unexpected reply: {other:?}"),
        }
    }

    /// run one tool. the text is what the agent reads back.
    async fn call(&mut self, name: &str, args: &Value) -> anyhow::Result<String> {
        let text = |v: Value| if v.is_null() { "done".to_owned() } else { serde_json::to_string_pretty(&v).unwrap_or_default() };
        Ok(match name {
            "photon_window" => {
                let cmd = args.get("cmd").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("photon_window needs \"cmd\""))?;
                text(self.window(cmd, args.get("args").cloned().unwrap_or(json!({}))).await?)
            }
            "photon_window_state" => text(self.window("ui.ls", json!({})).await?),
            "photon_commands" => {
                let all = self.window("commands.list", json!({})).await?;
                let mut out = String::new();
                let mut seen = std::collections::HashSet::new();
                for c in all.as_array().into_iter().flatten() {
                    let id = c["id"].as_str().unwrap_or("");
                    if seen.insert(id.to_owned()) {
                        out.push_str(&format!("{id}: {}{}\n", c["title"].as_str().unwrap_or(""), c["args"].as_str().filter(|a| !a.is_empty()).map(|a| format!(" — {a}")).unwrap_or_default()));
                    }
                }
                out
            }
            "photon_layout" => match args.get("json") {
                Some(json) => text(self.window("layout.set", json!({"json": json})).await?),
                None => text(self.window("layout.get", json!({})).await?),
            },
            "photon_surface" => {
                let session = self.session.ok_or_else(|| anyhow::anyhow!("surfaces belong to a session, and this agent wasn't started by photon"))?;
                let mut op = args.clone();
                if let Some(o) = op.as_object_mut() {
                    o.retain(|_, v| !v.is_null());
                }
                let op: SurfaceOp = serde_json::from_value(op).map_err(|e| anyhow::anyhow!("that isn't a surface op: {e}"))?;
                self.client().await?.request(Request::SurfaceOp { session, op }).await?;
                "done: it's on screen".to_owned()
            }
            other => anyhow::bail!("there's no tool {other}"),
        })
    }
}

/// serve until stdin closes.
pub async fn serve() -> anyhow::Result<()> {
    let session = std::env::var("PHOTON_SESSION").ok().and_then(|s| s.parse().ok());
    let mut server = Server { client: None, session };
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
                "serverInfo": {"name": "photon", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("").to_owned();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                Ok(match server.call(&name, &args).await {
                    Ok(text) => json!({"content": [{"type": "text", "text": text}], "isError": false}),
                    Err(e) => json!({"content": [{"type": "text", "text": format!("{e:#}")}], "isError": true}),
                })
            }
            other => Err(json!({"code": -32601, "message": format!("photon doesn't do {other}")})),
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
