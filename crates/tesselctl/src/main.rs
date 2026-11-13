//! tesselctl: drive the tessel daemon from a terminal.
//!
//! everything the app can do goes through the same socket, so everything the
//! app can do can be done (and tested) from here.

mod mcp;
mod shell;

use anyhow::{Context, bail};
use tessel_client::{Client, ClientError, Events};
use tessel_proto::*;
use std::path::PathBuf;
use uuid::Uuid;

const HELP: &str = "tesselctl: drive the tessel daemon

  tesselctl version                         client and daemon versions
  tesselctl ls [--json]                     list sessions
  tesselctl new [options] [prompt...]       create a session, print its id
      --cwd DIR        where it runs (default: here)
      --worktree       give it a fresh git worktree of the repo at --cwd
      --base REF       branch/ref the worktree starts from
      --name NAME      worktree and branch name
      --goal TEXT      the one-line \"what is this\"
      --model MODEL
  tesselctl send SESSION text...            send a message
  tesselctl events SESSION [--follow] [--json]   show the transcript, then live events
  tesselctl watch [--json]                  live summary of every session
  tesselctl approve SESSION [REQUEST]       allow a pending request (default: the oldest)
  tesselctl deny SESSION [REQUEST] [--message TEXT]
  tesselctl interrupt SESSION
  tesselctl seen SESSION                    mark as looked at
  tesselctl goal SESSION text...
  tesselctl model SESSION MODEL             switch model (an alias like sonnet, or a full id)
  tesselctl mode SESSION MODE               default, acceptEdits, auto or plan
  tesselctl effort SESSION LEVEL            low, medium, high, xhigh, max or default
  tesselctl fast SESSION on|off             fast mode, where the model has it
  tesselctl link SESSION DIR                let the agent work in another folder too
  tesselctl visual SESSION on|off           watch the agent work: its Bash commands live in a terminal, browser steps paced
  tesselctl models                          the models the agent offers
  tesselctl kube [pods|deployments|services|nodes] [-n NS] [--context C] [--json]
  tesselctl kube contexts|namespaces        what the kubernetes module shows, through your kubectl
  tesselctl rm SESSION [--worktree]         delete (optionally with its worktree)
  tesselctl open PATH[:LINE[:COL]]... [--wait]   show files in the window, the caret at LINE;
                                            --wait returns once each is closed (0 saved or
                                            clean, 1 edits thrown away), so EDITOR='tesselctl open --wait'
  tesselctl daemon start|stop
  tesselctl daemon restart [--now] [--wait SECS]   a new daemon (the binary on disk now)
                                            takes over: terminals and agents keep running

  the window: if you can click it, you can script it.
  tesselctl ui ls [--json]                  what the window shows: panes, drawers, theme
  tesselctl ui commands                     every command the window takes
  tesselctl ui COMMAND [key=value...]       run one, e.g. ui pane.show module=git pane=@focused
      (values that read as json are json: worktree=true, hex=null)
  tesselctl ui split [right|down] [PANE]    split a pane (default: the focused one, to the right)
  tesselctl ui close|zoom [PANE]
  tesselctl ui show MODULE [PANE]           show a module in a pane
  tesselctl ui focus PANE
  tesselctl ui theme tessel|orchard
  tesselctl ui events [--filter KIND,...]   what windows do, as json lines
  tesselctl ui windows                      the open windows
  tesselctl ui shot [PATH]                  a picture of the window, as a PNG

  live ui in a session's chat (what agents draw with their tessel_surface tool):
  tesselctl surface SESSION put NAME JSON [--pane]
  tesselctl surface SESSION set NAME ELEMENT JSON
  tesselctl surface SESSION remove NAME
  tesselctl mcp                             tessel as MCP tools, on stdin/stdout (agents get this)

  a session's repository (what the git pane does):
  tesselctl git SESSION log [-n 30]         history across branches, newest first
  tesselctl git SESSION status              staged and not
  tesselctl git SESSION stage|unstage PATH...
  tesselctl git SESSION lines PATH 4-9 [--unstage] [--old]
                                            stage (or unstage) only those lines
  tesselctl git SESSION commit MESSAGE...
  tesselctl git SESSION undo                take back the last thing tessel did here
  tesselctl git SESSION suggest             a commit message for what's staged

  terminals the daemon runs (ghostty's emulator; they outlive windows):
  tesselctl term ls
  tesselctl term open [--cwd DIR] [--session S] [--size 120x32] [-- COMMAND...]
                                            a login shell, or COMMAND run by one; prints its id
  tesselctl term send ID TEXT...            type TEXT (no return)
  tesselctl term run ID COMMAND...          type COMMAND and return
  tesselctl term capture ID [--history] [--ansi|--html]
                                            the text on screen (with --history, all of it), plain,
                                            with its colours as escape sequences, or as HTML
  tesselctl term process ID [--json]        what's running in it now: pid, command, folder
  tesselctl term keep-open ID on|off        stay once its program exits (on, the default), or go
  tesselctl term wait ID [--text T] [--timeout SECS]
                                            until the screen shows T, or until it exits
  tesselctl term attach ID                  its output, raw, as it comes
  tesselctl term close ID

  tesselctl settings [key=value...]         the daemon's settings; with pairs, change them
      e.g. settings agent_tools=false     agents no longer get tessel's tools
      --window NAME    which window (default: the one opened last)
      PANE is a pane id, @focused, or the module it shows

SESSION may be any unique prefix of a session id.
global: --no-start   do not start a daemon if none is running
env:    TESSEL_HOME  which daemon (default ~/.tessel)";

fn main() {
    // as a visual-mode agent's shell prefix: before anything else, so the
    // shells it only passes on cost one exec and nothing more.
    let args: Vec<String> = std::env::args().collect();
    if args.first().and_then(|a| a.rsplit('/').next()) == Some(shell::NAME) {
        shell::main(args);
    }
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("a runtime");
    if let Err(e) = rt.block_on(run()) {
        eprintln!("tesselctl: {e:#}");
        std::process::exit(1);
    }
}

struct Args {
    rest: Vec<String>,
}

impl Args {
    /// remove `--flag` if present.
    fn flag(&mut self, name: &str) -> bool {
        match self.rest.iter().position(|a| a == name) {
            Some(i) => {
                self.rest.remove(i);
                true
            }
            None => false,
        }
    }

    /// remove `--name VALUE` if present.
    fn opt(&mut self, name: &str) -> anyhow::Result<Option<String>> {
        match self.rest.iter().position(|a| a == name) {
            Some(i) if i + 1 < self.rest.len() => {
                self.rest.remove(i);
                Ok(Some(self.rest.remove(i)))
            }
            Some(_) => bail!("{name} needs a value"),
            None => Ok(None),
        }
    }

    fn next(&mut self, what: &str) -> anyhow::Result<String> {
        if self.rest.is_empty() {
            bail!("missing {what} (see tesselctl --help)");
        }
        Ok(self.rest.remove(0))
    }
}

async fn run() -> anyhow::Result<()> {
    tessel_proto::local::move_from_photon(&tessel_proto::local::home()).map_err(anyhow::Error::msg)?;
    let mut args = Args { rest: std::env::args().skip(1).collect() };
    if args.rest.is_empty() || args.flag("--help") || args.flag("-h") {
        println!("{HELP}");
        return Ok(());
    }
    let no_start = args.flag("--no-start");
    let json = args.flag("--json");
    let home = tessel_proto::local::home();
    let cmd = args.next("a command")?;

    // tessel as tools for an agent: connects only when a tool is used.
    if cmd == "mcp" {
        return mcp::serve().await;
    }

    if cmd == "daemon" {
        return match args.next("start, stop or restart")?.as_str() {
            "restart" => {
                let now = args.flag("--now");
                let wait_secs = args.opt("--wait")?.map(|s| s.parse()).transpose()?;
                let (c, _, w) = Client::connect(&tessel_proto::local::socket(&home), "tesselctl").await?;
                if !w.capabilities.iter().any(|c| c == "handoff") {
                    bail!("this daemon (pid {}) can't hand over to a new one; `tesselctl daemon stop` stops it, and what it runs with it", w.daemon_pid);
                }
                // agents mid-turn are waited for: say so, so a wait isn't a mystery.
                if !now
                    && let Ok(Response::Sessions { sessions }) = c.request(Request::SessionList).await
                {
                    let busy = sessions.iter().filter(|s| matches!(s.run_state, tessel_proto::RunState::Working | tessel_proto::RunState::Starting) || s.attention.as_ref().is_some_and(|a| matches!(a.reason, tessel_proto::AttentionReason::NeedsPermission | tessel_proto::AttentionReason::NeedsInput))).count();
                    if busy > 0 {
                        eprintln!("waiting for {busy} agent(s) to finish their turn (--now cuts them short)");
                    }
                }
                let bin = tessel_client::daemon_binary();
                let bin = bin.is_absolute().then(|| bin.to_string_lossy().into_owned());
                match c.request(Request::DaemonRestart { now, wait_secs, bin }).await? {
                    Response::Restarted { pid, terminals, agents, cut_short } => {
                        println!("restarted: tesseld pid {} took over from pid {}", pid, w.daemon_pid);
                        println!("  kept running: {terminals} terminal(s), {agents} agent(s)");
                        if cut_short > 0 {
                            println!("  cut short mid-turn: {cut_short} agent(s); they pick up again from their conversations");
                        }
                    }
                    other => bail!("unexpected reply {other:?}"),
                }
                Ok(())
            }
            "start" => {
                let (_, _, w) = tessel_client::connect_or_start(&home, "tesselctl").await?;
                println!("tesseld {} running, pid {}", w.build, w.daemon_pid);
                Ok(())
            }
            "stop" => match Client::connect(&tessel_proto::local::socket(&home), "tesselctl").await {
                Ok((c, _, w)) => {
                    // the daemon may close the connection before the reply lands; either way it's stopping.
                    let _ = c.request(Request::Shutdown).await;
                    println!("stopped tesseld pid {}", w.daemon_pid);
                    Ok(())
                }
                Err(ClientError::Connect { .. }) => {
                    println!("no daemon running");
                    Ok(())
                }
                Err(e) => Err(e.into()),
            },
            other => bail!("unknown daemon command {other}"),
        };
    }

    let (client, mut events, welcome) = if no_start {
        Client::connect(&tessel_proto::local::socket(&home), "tesselctl").await?
    } else {
        tessel_client::connect_or_start(&home, "tesselctl").await?
    };

    match cmd.as_str() {
        "version" => {
            println!("tesselctl {} (protocol {PROTO_MAJOR}.{PROTO_MINOR})", env!("CARGO_PKG_VERSION"));
            println!("tesseld   {} (protocol {}.{}) pid {}", welcome.build, welcome.proto_major, welcome.proto_minor, welcome.daemon_pid);
        }
        "ls" => {
            let Response::Sessions { sessions } = client.request(Request::SessionList).await? else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&sessions)?);
            } else if sessions.is_empty() {
                println!("no sessions");
            } else {
                for s in &sessions {
                    println!("{}", summary_line(s));
                }
            }
        }
        "new" => {
            let cwd = match args.opt("--cwd")? {
                Some(c) => PathBuf::from(c),
                None => std::env::current_dir()?,
            };
            let cwd = cwd.canonicalize().with_context(|| format!("{} does not exist", cwd.display()))?;
            let want_worktree = args.flag("--worktree");
            let base = args.opt("--base")?;
            let name = args.opt("--name")?;
            let worktree = (want_worktree || base.is_some() || name.is_some()).then_some(WorktreeSpec { base, name });
            let goal = args.opt("--goal")?;
            let model = args.opt("--model")?;
            let prompt = (!args.rest.is_empty()).then(|| args.rest.join(" "));
            let req = Request::SessionCreate {
                cmd_id: Uuid::new_v4(),
                cwd: cwd.to_string_lossy().into_owned(),
                worktree,
                goal,
                prompt,
                model,
                parent: None, attachments: vec![]
            };
            let Response::Session { summary } = client.request(req).await? else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&summary)?);
            } else {
                println!("{}", summary.id);
            }
        }
        "ui" => ui(&client, &mut events, &mut args, json).await?,
        "surface" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let pane = args.flag("--pane");
            let op = match args.next("put, set or remove")?.as_str() {
                "put" => tessel_proto::surface::SurfaceOp::Put {
                    surface: args.next("a surface name")?,
                    node: serde_json::from_str(&args.next("the node, as json")?).context("the node isn't json")?,
                    region: if pane { "pane".into() } else { "inline".into() },
                },
                "set" => tessel_proto::surface::SurfaceOp::Set {
                    surface: args.next("a surface name")?,
                    element: args.next("an element id")?,
                    props: serde_json::from_str(&args.next("the props, as json")?).context("the props aren't json")?,
                },
                "remove" => tessel_proto::surface::SurfaceOp::Remove { surface: args.next("a surface name")? },
                other => bail!("unknown surface op {other}: use put, set or remove"),
            };
            client.request(Request::SurfaceOp { session, op }).await?;
        }
        "term" => term(&client, &mut events, &mut args, json).await?,
        "git" => git(&client, &mut args, json).await?,
        "settings" => {
            let rest: Vec<String> = std::mem::take(&mut args.rest);
            let reply = if rest.is_empty() { client.request(Request::Settings).await? } else { client.request(Request::SetSettings { patch: ui_args(&rest)? }).await? };
            let Response::Settings { settings } = reply else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&settings)?);
            } else {
                for (k, v) in serde_json::to_value(&settings)?.as_object().into_iter().flatten() {
                    println!("{k} = {v}");
                }
            }
        }
        "open" => {
            let wait = args.flag("--wait");
            if args.rest.is_empty() {
                bail!("open needs a file: tesselctl open PATH[:LINE[:COL]]");
            }
            let Response::Sessions { sessions } = client.request(Request::SessionList).await? else { bail!("unexpected reply") };
            let here = std::env::var("TESSEL_SESSION").ok().and_then(|s| s.parse::<tessel_proto::SessionId>().ok());
            if wait {
                client.request(Request::UiSubscribe { kinds: vec!["file_closed".into()] }).await?;
            }
            let mut code = 0;
            for spec in std::mem::take(&mut args.rest) {
                let (path, line, col) = file_spec(&spec);
                let full = std::fs::canonicalize(&path).map_err(|e| anyhow::anyhow!("{path}: {e}"))?;
                // the session whose folder it's in: this terminal's own first, else the closest.
                let within = |cwd: &str| std::fs::canonicalize(cwd).ok().and_then(|c| full.strip_prefix(&c).ok().map(|r| (c.components().count(), r.to_string_lossy().into_owned())));
                let found = here
                    .and_then(|h| sessions.iter().find(|s| s.id == h))
                    .and_then(|s| within(&s.cwd).map(|(_, rel)| (s.id, rel)))
                    .or_else(|| sessions.iter().filter_map(|s| within(&s.cwd).map(|(depth, rel)| (depth, s.id, rel))).max_by_key(|(d, ..)| *d).map(|(_, id, rel)| (id, rel)));
                let Some((session, rel)) = found else {
                    bail!("{} isn't in any session's folder; tessel opens files where its sessions work", full.display());
                };
                let mut a = serde_json::json!({"path": rel, "session": session.to_string()});
                if let Some(l) = line {
                    a["line"] = l.into();
                    a["col"] = col.unwrap_or(1).into();
                }
                ui_exec(&client, &None, "files.open", a).await?;
                if !wait {
                    continue;
                }
                // until it's closed in the window: 1 if edits were thrown away, as editors say.
                loop {
                    let Some(ev) = events.recv().await else { bail!("the daemon went away") };
                    if let Event::Ui { kind, data, .. } = ev
                        && kind == "file_closed"
                        && data["path"] == rel
                        && data["session"] == session.to_string()
                    {
                        if data["discarded"].as_bool() == Some(true) {
                            code = 1;
                        }
                        break;
                    }
                }
            }
            if code != 0 {
                std::process::exit(code);
            }
        }
        "send" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let text = args.rest.join(" ");
            client.request(Request::SessionSend { cmd_id: Uuid::new_v4(), session, text, attachments: Vec::new() }).await?;
        }
        "events" => {
            let follow = args.flag("--follow") || args.flag("-f");
            let session = resolve(&client, &args.next("a session")?).await?;
            let Response::Subscribed { detail: Some(snap), .. } =
                client.request(Request::Subscribe { summary: false, detail: Some(session), tail: 10_000 }).await?
            else {
                bail!("unexpected reply")
            };
            if json {
                println!("{}", serde_json::to_string(&snap)?);
            } else {
                for item in &snap.items {
                    println!("{}", item_line(item));
                }
                for p in &snap.pending {
                    println!("{}", pending_line(p));
                }
                println!("{}", summary_line(&snap.summary));
            }
            if follow {
                follow_events(&mut events, json).await;
            }
        }
        "watch" => {
            let Response::Subscribed { sessions: Some(sessions), .. } =
                client.request(Request::Subscribe { summary: true, detail: None, tail: 0 }).await?
            else {
                bail!("unexpected reply")
            };
            for s in &sessions {
                if json { println!("{}", serde_json::to_string(s)?) } else { println!("{}", summary_line(s)) }
            }
            follow_events(&mut events, json).await;
        }
        "approve" | "deny" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let message = args.opt("--message")?.unwrap_or_default();
            let Response::Subscribed { detail: Some(snap), .. } =
                client.request(Request::Subscribe { summary: false, detail: Some(session), tail: 0 }).await?
            else {
                bail!("unexpected reply")
            };
            let request = match args.rest.first() {
                Some(id) => snap.pending.iter().find(|p| p.request_id.0.starts_with(id.as_str())).with_context(|| format!("no pending request {id}"))?,
                None => snap.pending.first().context("nothing is waiting on you in that session")?,
            };
            let decision = if cmd == "approve" { PermissionDecision::Allow { updated_input: None } } else { PermissionDecision::Deny { message } };
            client
                .request(Request::PermissionRespond { cmd_id: Uuid::new_v4(), session, request_id: request.request_id.clone(), decision })
                .await?;
            println!("{} {}", if cmd == "approve" { "allowed" } else { "denied" }, pending_line(request));
        }
        "interrupt" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionInterrupt { cmd_id: Uuid::new_v4(), session }).await?;
        }
        "seen" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::MarkSeen { session, seq: u64::MAX }).await?;
        }
        "goal" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionSetGoal { cmd_id: Uuid::new_v4(), session, goal: args.rest.join(" ") }).await?;
        }
        "model" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionSetModel { cmd_id: Uuid::new_v4(), session, model: args.next("a model")? }).await?;
        }
        "mode" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionSetMode { cmd_id: Uuid::new_v4(), session, mode: args.next("a mode")? }).await?;
        }
        "effort" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let level = args.next("an effort level")?;
            let effort = (level != "default").then_some(level);
            client.request(Request::SessionSetEffort { cmd_id: Uuid::new_v4(), session, effort }).await?;
        }
        "fast" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let fast = match args.next("on or off")?.as_str() {
                "on" | "true" => true,
                "off" | "false" => false,
                other => bail!("fast is on or off, not {other}"),
            };
            client.request(Request::SessionSetFast { cmd_id: Uuid::new_v4(), session, fast }).await?;
        }
        "visual" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            let on = |w: &str| match w {
                "on" => Ok(true),
                "off" => Ok(false),
                other => Err(anyhow::anyhow!("on or off, not {other}")),
            };
            let visual = on(&args.next("on or off")?)?;
            client.request(Request::SessionSetVisual { session, visual }).await?;
        }
        "link" => {
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionLinkDir { cmd_id: Uuid::new_v4(), session, path: args.next("a folder")? }).await?;
        }
        "kube" => {
            let context = args.opt("--context")?;
            let namespace = args.opt("-n")?.or(args.opt("--namespace")?);
            let json = args.flag("--json");
            let what = args.rest.first().cloned().unwrap_or_else(|| "pods".into());
            match what.as_str() {
                "contexts" => match client.request(Request::KubeContexts).await? {
                    Response::KubeContexts { current, contexts } => {
                        for c in contexts {
                            println!("{} {c}", if current.as_deref() == Some(c.as_str()) { "*" } else { " " });
                        }
                    }
                    other => bail!("unexpected reply: {other:?}"),
                },
                "namespaces" | "ns" => match client.request(Request::KubeNamespaces { context }).await? {
                    Response::KubeNamespaces { namespaces } => namespaces.iter().for_each(|n| println!("{n}")),
                    other => bail!("unexpected reply: {other:?}"),
                },
                kind => {
                    let kind: kube::KubeKind = serde_json::from_value(serde_json::Value::String(kind.to_owned()))?;
                    if kind == kube::KubeKind::Unknown {
                        bail!("pods, deployments, services, nodes, contexts or namespaces, not {what}");
                    }
                    match client.request(Request::KubeList { context, namespace, kind }).await? {
                        Response::KubeObjects { objects, .. } if json => println!("{}", serde_json::to_string_pretty(&objects)?),
                        Response::KubeObjects { objects, metrics, .. } => {
                            for o in objects {
                                let ns = o.namespace.map(|n| format!("{n}/")).unwrap_or_default();
                                let usage = if metrics {
                                    format!("{:>6} {:>8}", o.cpu_milli.map(|c| format!("{c}m")).unwrap_or_default(), o.mem_bytes.map(|b| format!("{}Mi", b / 1048576)).unwrap_or_default())
                                } else {
                                    String::new()
                                };
                                println!("{:<48} {:<22} {:>6} {:>3} {usage}", format!("{ns}{}", o.name), o.status, o.ready.unwrap_or_default(), o.restarts.map(|r| r.to_string()).unwrap_or_default());
                            }
                        }
                        other => bail!("unexpected reply: {other:?}"),
                    }
                }
            }
        }
        "models" => match client.request(Request::ModelList).await? {
            Response::Models { models, .. } => {
                for m in models {
                    let fast = if m.fast { "  fast" } else { "" };
                    println!("{:<28} {}  {}  [{}]{fast}", m.value, m.name, m.description, m.efforts.join(" "));
                }
            }
            other => bail!("unexpected reply: {other:?}"),
        },
        "rm" => {
            let remove_worktree = args.flag("--worktree");
            let session = resolve(&client, &args.next("a session")?).await?;
            client.request(Request::SessionDelete { cmd_id: Uuid::new_v4(), session, remove_worktree }).await?;
        }
        other => bail!("unknown command {other} (see tesselctl --help)"),
    }
    Ok(())
}

/// a full id, or any prefix that names exactly one session.
async fn resolve(client: &Client, prefix: &str) -> anyhow::Result<SessionId> {
    if let Ok(id) = prefix.parse::<SessionId>() {
        return Ok(id);
    }
    let Response::Sessions { sessions } = client.request(Request::SessionList).await? else { bail!("unexpected reply") };
    let hits: Vec<_> = sessions.iter().filter(|s| s.id.to_string().starts_with(prefix)).collect();
    match hits.as_slice() {
        [one] => Ok(one.id),
        [] => bail!("no session starts with {prefix}"),
        many => bail!("{prefix} matches {} sessions; use more of the id", many.len()),
    }
}

/// `key=value` to json: a value that reads as json is json, anything else is text.
fn ui_args(pairs: &[String]) -> anyhow::Result<serde_json::Value> {
    let mut map = serde_json::Map::new();
    for p in pairs {
        let Some((k, v)) = p.split_once('=') else { bail!("\"{p}\" should be key=value") };
        let value = serde_json::from_str::<serde_json::Value>(v).unwrap_or_else(|_| serde_json::Value::String(v.to_owned()));
        map.insert(k.to_owned(), value);
    }
    Ok(serde_json::Value::Object(map))
}

/// `PATH[:LINE[:COL]]`, where a path that exists as written wins (colons and all).
fn file_spec(spec: &str) -> (String, Option<u32>, Option<u32>) {
    if std::path::Path::new(spec).exists() {
        return (spec.to_owned(), None, None);
    }
    let mut parts = spec.rsplitn(3, ':').collect::<Vec<_>>();
    parts.reverse();
    match parts.as_slice() {
        [p, l, c] if l.parse::<u32>().is_ok() && c.parse::<u32>().is_ok() => ((*p).to_owned(), l.parse().ok(), c.parse().ok()),
        [p, l, c] if c.parse::<u32>().is_ok() => (format!("{p}:{l}"), c.parse().ok(), None),
        [p, l] if l.parse::<u32>().is_ok() => ((*p).to_owned(), l.parse().ok(), None),
        _ => (spec.to_owned(), None, None),
    }
}

async fn ui_exec(client: &Client, window: &Option<String>, cmd: &str, args: serde_json::Value) -> anyhow::Result<serde_json::Value> {
    match client.request(Request::UiExec { window: window.clone(), cmd: cmd.to_owned(), args }).await? {
        Response::UiResult { value } => Ok(value),
        other => bail!("unexpected reply: {other:?}"),
    }
}

async fn ui(client: &Client, events: &mut tessel_client::Events, args: &mut Args, json: bool) -> anyhow::Result<()> {
    let window = args.opt("--window")?;
    let verb = args.next("a window command (see tesselctl --help)")?;
    let print = |v: &serde_json::Value| {
        if !v.is_null() {
            println!("{}", if json { v.to_string() } else { serde_json::to_string_pretty(v).unwrap_or_default() });
        }
    };
    let rest = std::mem::take(&mut args.rest);
    let positional = |i: usize| rest.get(i).cloned();
    let pane = |i: usize| positional(i).map(|p| serde_json::json!({"pane": p})).unwrap_or(serde_json::json!({}));
    let with = |mut base: serde_json::Value, key: &str, value: Option<String>| {
        if let (Some(v), Some(o)) = (value, base.as_object_mut()) {
            o.insert(key.to_owned(), serde_json::Value::String(v));
        }
        base
    };
    match verb.as_str() {
        "shot" => {
            let mut a = serde_json::json!({});
            if let Some(p) = positional(0) {
                let p = std::path::absolute(&p)?;
                a["path"] = serde_json::Value::String(p.to_string_lossy().into_owned());
            }
            let v = ui_exec(client, &window, "ui.shot", a).await?;
            println!("{}", v["path"].as_str().unwrap_or_default());
        }
        "windows" => match client.request(Request::UiWindows).await? {
            Response::UiWindows { windows } if windows.is_empty() => println!("no windows open"),
            Response::UiWindows { windows } => windows.iter().for_each(|w| println!("{w}")),
            other => bail!("unexpected reply: {other:?}"),
        },
        "events" => {
            let kinds = args.opt("--filter")?.map(|f| f.split(',').map(|k| k.trim().to_owned()).collect()).unwrap_or_default();
            client.request(Request::UiSubscribe { kinds }).await?;
            while let Some(ev) = events.recv().await {
                if let Event::Ui { window, kind, data } = ev {
                    println!("{}", serde_json::json!({"window": window, "kind": kind, "data": data}));
                }
            }
        }
        "ls" => {
            let v = ui_exec(client, &window, "ui.ls", serde_json::json!({})).await?;
            if json {
                println!("{v}");
            } else {
                match v.get("session").filter(|s| !s.is_null()) {
                    Some(s) => println!("session  {}  {}", s["goal"].as_str().unwrap_or(""), s["id"].as_str().unwrap_or("").get(..8).unwrap_or("")),
                    None => println!("session  (none open)"),
                }
                for p in v["panes"].as_array().into_iter().flatten() {
                    let flags = [("focused", "*"), ("zoomed", "maximized"), ("open", "")]
                        .iter()
                        .filter(|(k, _)| p[*k].as_bool() == Some(true) && !(*k == "open"))
                        .map(|(_, f)| *f)
                        .collect::<Vec<_>>()
                        .join(" ");
                    let shown = if p["open"].as_bool() == Some(false) { "folded" } else { "" };
                    println!("pane     {:<14} {:<12} {} {}", p["id"].as_str().unwrap_or(""), p["module"].as_str().unwrap_or("(empty)"), flags, shown);
                }
                for d in v["drawers"].as_array().into_iter().flatten() {
                    println!("drawer   {:<14} {}", d["id"].as_str().unwrap_or(""), if d["open"].as_bool() == Some(true) { "open" } else { "shut" });
                }
                println!("theme    {} {}", v["theme"]["base"].as_str().unwrap_or(""), if v["theme"]["dark"].as_bool() == Some(true) { "dark" } else { "light" });
            }
        }
        "commands" => {
            let v = ui_exec(client, &window, "commands.list", serde_json::json!({})).await?;
            if json {
                println!("{v}");
            } else {
                let mut seen = std::collections::HashSet::new();
                for c in v.as_array().into_iter().flatten() {
                    let id = c["id"].as_str().unwrap_or("");
                    if seen.insert(id.to_owned()) {
                        println!("{id:<18} {}", c["args"].as_str().filter(|a| !a.is_empty()).unwrap_or("-"));
                    }
                }
            }
        }
        "split" => {
            let (dir, at) = match positional(0).as_deref() {
                Some("right" | "down") => (positional(0), 1),
                _ => (Some("right".into()), 0),
            };
            print(&ui_exec(client, &window, "pane.split", with(pane(at), "dir", dir)).await?);
        }
        "close" => print(&ui_exec(client, &window, "pane.close", pane(0)).await?),
        "zoom" => print(&ui_exec(client, &window, "pane.zoom", pane(0)).await?),
        "focus" => print(&ui_exec(client, &window, "pane.focus", pane(0)).await?),
        "show" => print(&ui_exec(client, &window, "pane.show", with(pane(1), "module", positional(0))).await?),
        "theme" => print(&ui_exec(client, &window, "theme.set", with(serde_json::json!({}), "base", positional(0))).await?),
        cmd => print(&ui_exec(client, &window, cmd, ui_args(&rest)?).await?),
    }
    Ok(())
}

async fn follow_events(events: &mut Events, json: bool) {
    while let Some(ev) = events.recv().await {
        if json {
            if let Ok(line) = serde_json::to_string(&ev) {
                println!("{line}");
            }
            continue;
        }
        match ev {
            Event::SessionUpdated { summary } => println!("{}", summary_line(&summary)),
            Event::SessionRemoved { session } => println!("removed {session}"),
            Event::ItemUpserted { item, .. } => println!("{}", item_line(&item)),
            Event::ItemDelta { text, .. } => println!("  + {}", one_line(&text, 100)),
            Event::PendingChanged { pending, .. } => {
                if pending.is_empty() {
                    println!("  (nothing pending)");
                }
                for p in &pending {
                    println!("{}", pending_line(p));
                }
            }
            Event::Lagged => println!("!! fell behind; events were dropped. run again for a fresh view."),
            Event::Ui { window, kind, .. } => println!("window {window}: {kind}"),
            Event::Settings { settings } => println!("settings: {}", serde_json::to_string(&settings).unwrap_or_default()),
            Event::TermUpdated { info } => println!("{}", term_line(&info)),
            Event::TermRemoved { term } => println!("terminal {term} closed"),
            Event::TermOutput { .. } => {}
            Event::BrowserActivity { session, what, .. } => println!("{session} browser: {what}"),
            Event::BrowserPicked { session, picked } => println!("{session} browser: picked {} on {}", picked.selector, picked.url),
            Event::Usage { windows } => {
                for w in windows {
                    println!("usage: {} {:.0}% ({})", w.name, w.used * 100.0, w.status);
                }
            }
            Event::BrowserState { .. } | Event::BrowserFrame { .. } | Event::BrowserEngine { .. } => {}
            Event::UiCall { .. } | Event::BrowserCall { .. } | Event::UiWatched { .. } | Event::Unknown => {}
        }
    }
    println!("connection closed");
}

fn tree_lines(t: &tessel_proto::WorkTree) {
    let mark = |s: &ChangeStatus| match s {
        ChangeStatus::Added => "A",
        ChangeStatus::Modified => "M",
        ChangeStatus::Deleted => "D",
        ChangeStatus::Untracked => "U",
        ChangeStatus::Unknown => "?",
    };
    println!("staged:");
    for f in &t.staged {
        println!("  {} {}  +{} -{}", mark(&f.status), f.path, f.added, f.removed);
    }
    println!("not staged:");
    for f in &t.unstaged {
        println!("  {} {}  +{} -{}", mark(&f.status), f.path, f.added, f.removed);
    }
    if let Some(u) = &t.undo {
        println!("undo would take back: {u}");
    }
}

async fn git(client: &Client, args: &mut Args, json: bool) -> anyhow::Result<()> {
    let session = resolve(client, &args.next("a session")?).await?;
    let tree = |r: Response, json: bool| -> anyhow::Result<()> {
        let Response::WorkTree { tree } = r else { bail!("unexpected reply") };
        if json {
            println!("{}", serde_json::to_string(&tree)?);
        } else {
            tree_lines(&tree);
        }
        Ok(())
    };
    match args.next("log, status, stage, unstage, lines, commit, undo or suggest")?.as_str() {
        "log" => {
            let limit = args.opt("-n")?.map(|n| n.parse()).transpose().context("-n is a number")?.unwrap_or(30);
            let Response::GitLog { log } = client.request(Request::GitLog { session, skip: 0, limit }).await? else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&log)?);
            } else {
                for c in &log.commits {
                    let refs = if c.refs.is_empty() { String::new() } else { format!(" ({})", c.refs.join(", ")) };
                    println!("{} {}{refs}", &c.sha[..c.sha.len().min(8)], c.subject);
                }
            }
        }
        "status" => tree(client.request(Request::GitWorkTree { session }).await?, json)?,
        verb @ ("stage" | "unstage") => {
            let paths = std::mem::take(&mut args.rest);
            let req = if verb == "stage" { Request::GitStage { session, paths } } else { Request::GitUnstage { session, paths } };
            tree(client.request(req).await?, json)?;
        }
        "lines" => {
            // tesselctl git S lines PATH 4-9 [--unstage]
            let unstage = args.flag("--unstage");
            let old = args.flag("--old");
            let path = args.next("a file")?;
            let range = args.next("lines, like 4-9")?;
            let (start, end) = match range.split_once('-') {
                Some((a, b)) => (a.parse()?, b.parse()?),
                None => (range.parse()?, range.parse()?),
            };
            tree(client.request(Request::GitStageLines { session, path, start, end, unstage, old }).await?, json)?;
        }
        "commit" => {
            let message = std::mem::take(&mut args.rest).join(" ");
            let Response::Committed { sha } = client.request(Request::GitCommit { session, message }).await? else { bail!("unexpected reply") };
            println!("{sha}");
        }
        "undo" => tree(client.request(Request::GitUndo { session }).await?, json)?,
        "suggest" => {
            let Response::Suggested { message } = client.request(Request::GitSuggestMessage { session }).await? else { bail!("unexpected reply") };
            println!("{message}");
        }
        other => bail!("unknown git command {other}: use log, status, stage, unstage, lines, commit, undo or suggest"),
    }
    Ok(())
}

fn term_line(t: &tessel_proto::term::TermInfo) -> String {
    let what = if t.command.is_empty() { "shell".to_owned() } else { t.command.join(" ") };
    let state = t.exited.map_or_else(|| "running".to_owned(), |c| format!("exited {c}"));
    let title = if t.title.is_empty() { String::new() } else { format!("  {}", t.title) };
    format!("{:<4} {:<10} {:>3}x{:<3} {}  {}{title}", t.id, state, t.cols, t.rows, t.cwd, what)
}

async fn term(client: &Client, events: &mut Events, args: &mut Args, json: bool) -> anyhow::Result<()> {
    let id = |s: String| s.parse::<tessel_proto::term::TermId>().with_context(|| format!("{s} isn't a terminal id (tesselctl term ls lists them)"));
    match args.next("ls, open, send, run, capture, wait, attach or close")?.as_str() {
        "ls" => {
            let Response::Terms { terms } = client.request(Request::TermList).await? else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&terms)?);
            }
            for t in terms.iter().filter(|_| !json) {
                println!("{}", term_line(t));
            }
        }
        "open" => {
            let cwd = args.opt("--cwd")?.map(|c| PathBuf::from(c).canonicalize().map(|p| p.to_string_lossy().into_owned())).transpose()?;
            let session = match args.opt("--session")? {
                Some(s) => Some(resolve(client, &s).await?),
                None => None,
            };
            let size = args.opt("--size")?.unwrap_or_else(|| "120x32".into());
            let (cols, rows) = size.split_once('x').and_then(|(c, r)| Some((c.parse().ok()?, r.parse().ok()?))).context("--size is COLSxROWS, like 120x32")?;
            if args.rest.first().is_some_and(|a| a == "--") {
                args.rest.remove(0);
            }
            let command = std::mem::take(&mut args.rest);
            let Response::Term { info } = client.request(Request::TermOpen { session, cwd, command, cols, rows, agent: false }).await? else { bail!("unexpected reply") };
            if json {
                println!("{}", serde_json::to_string(&info)?);
            } else {
                println!("{}", info.id);
            }
        }
        verb @ ("send" | "run") => {
            let term = id(args.next("a terminal")?)?;
            let mut text = std::mem::take(&mut args.rest).join(" ");
            if verb == "run" {
                text.push('\r');
            }
            client.request(Request::TermInput { term, text }).await?;
        }
        "capture" => {
            let term = id(args.next("a terminal")?)?;
            let history = args.flag("--history");
            let format = if args.flag("--html") {
                Some("html".to_owned())
            } else if args.flag("--ansi") {
                Some("ansi".to_owned())
            } else {
                None
            };
            let Response::TermText { text } = client.request(Request::TermCapture { term, history, format }).await? else { bail!("unexpected reply") };
            println!("{text}");
        }
        "process" => {
            let term = id(args.next("a terminal")?)?;
            match client.request(Request::TermProcess { term }).await? {
                Response::TermProcess { pid: Some(pid), command, cwd } if json => println!("{}", serde_json::json!({"pid": pid, "command": command, "cwd": cwd})),
                Response::TermProcess { pid: Some(pid), command, cwd } => println!("{pid}  {command}{}", cwd.map(|c| format!("  (in {c})")).unwrap_or_default()),
                Response::TermProcess { pid: None, .. } => println!("nothing: it has exited"),
                other => bail!("unexpected reply: {other:?}"),
            }
        }
        "keep-open" => {
            let term = id(args.next("a terminal")?)?;
            let on = match args.next("on or off")?.as_str() {
                "on" => true,
                "off" => false,
                other => bail!("keep-open is on or off, not {other}"),
            };
            client.request(Request::TermKeepOpen { term, on }).await?;
            println!("terminal {term} {}", if on { "stays once its program exits" } else { "goes once its program exits" });
        }
        "wait" => {
            let term = id(args.next("a terminal")?)?;
            let text = args.opt("--text")?;
            let secs: f64 = args.opt("--timeout")?.map(|t| t.parse()).transpose().context("--timeout is seconds")?.unwrap_or(30.0);
            match client.request(Request::TermWait { term, text, timeout_ms: (secs * 1000.0) as u64 }).await? {
                Response::TermText { text } => println!("{text}"),
                Response::Term { info } => {
                    println!("{}", term_line(&info));
                    if let Some(code) = info.exited.filter(|c| *c != 0) {
                        std::process::exit(code.clamp(1, 255));
                    }
                }
                other => bail!("unexpected reply: {other:?}"),
            }
        }
        "attach" => {
            // what it shows and then what it writes, raw, until it closes. read-only.
            let term = id(args.next("a terminal")?)?;
            client.request(Request::TermAttach { term }).await?;
            use std::io::Write as _;
            let mut out = std::io::stdout();
            while let Some(ev) = events.recv().await {
                match ev {
                    Event::TermOutput { term: t, bytes } if t == term => {
                        out.write_all(&bytes)?;
                        out.flush()?;
                    }
                    Event::TermRemoved { term: t } if t == term => break,
                    _ => {}
                }
            }
        }
        "close" => {
            let term = id(args.next("a terminal")?)?;
            client.request(Request::TermClose { term }).await?;
        }
        other => bail!("unknown term command {other}: use ls, open, send, run, capture, wait, attach or close"),
    }
    Ok(())
}

fn run_state_word(s: &RunState) -> &'static str {
    match s {
        RunState::Starting => "starting",
        RunState::Working => "working",
        RunState::NeedsPermission => "NEEDS PERMISSION",
        RunState::NeedsInput => "NEEDS INPUT",
        RunState::Idle => "idle",
        RunState::Stalled => "STALLED",
        RunState::Limited { .. } => "usage limit",
        RunState::Errored => "ERRORED",
        RunState::Unknown => "?",
    }
}

fn summary_line(s: &SessionSummary) -> String {
    let id = &s.id.to_string()[..8];
    let attention = match &s.attention {
        Some(a) => format!(" [wants you: {}]", serde_json::to_value(a.reason).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()),
        None => String::new(),
    };
    let activity = s.activity.as_deref().map(|a| format!(" · {a}")).unwrap_or_default();
    let place = match &s.worktree {
        Some(w) => format!("{} ({})", s.cwd, w.branch),
        None => s.cwd.clone(),
    };
    format!("{id}  {:<16} {}{activity}{attention}\n          {place}  ${:.4}", run_state_word(&s.run_state), s.goal, s.cost_usd)
}

fn item_line(item: &Item) -> String {
    let indent = if item.parent.is_some() { "    " } else { "" };
    match &item.body {
        ItemBody::User { text, .. } => format!("{indent}you: {}", one_line(text, 200)),
        ItemBody::Text { text, streaming } => format!("{indent}agent{}: {}", if *streaming { " (writing)" } else { "" }, one_line(text, 200)),
        ItemBody::Thinking { streaming, .. } => format!("{indent}(thinking{})", if *streaming { "…" } else { "" }),
        ItemBody::Tool { tool, input, status, output, .. } => {
            let status = serde_json::to_value(status).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default();
            let out = output.as_deref().map(|o| format!(" -> {}", one_line(o, 120))).unwrap_or_default();
            format!("{indent}tool {tool} [{status}] {}{out}", one_line(&input.to_string(), 120))
        }
        ItemBody::Surface { surface, node, .. } => format!("  ▣ surface {surface}{}", if node.is_null() { " (removed)" } else { "" }),
        ItemBody::Turn { duration_ms, cost_usd, usage, files, .. } => {
            let took = duration_ms.map(|d| format!("{:.1}s", d as f64 / 1000.0)).unwrap_or_default();
            let cost = cost_usd.map(|c| format!(" ${c:.2}")).unwrap_or_default();
            let tokens = usage.map(|u| format!(" {} in, {} out, {} cached", u.input, u.output, u.cache_read)).unwrap_or_default();
            let changed = if files.is_empty() { String::new() } else { format!(" · {} file{} changed", files.len(), if files.len() == 1 { "" } else { "s" }) };
            format!("{indent}-- turn {took}{cost}{tokens}{changed} --")
        }
        ItemBody::Notice { text, .. } => format!("{indent}-- {text} --"),
        ItemBody::Unknown => format!("{indent}(something this build does not know how to show)"),
    }
}

fn pending_line(p: &PermissionRequest) -> String {
    let what = if p.is_question { "question" } else { "permission" };
    format!("  ?? {what} {} {} {}", &p.request_id.0[..p.request_id.0.len().min(8)], p.tool, one_line(&p.input.to_string(), 160))
}

fn one_line(s: &str, max: usize) -> String {
    let flat: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    flat.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_file_is_named_with_its_line_and_column_after_colons() {
        assert_eq!(super::file_spec("/nowhere/src/main.rs:12:4"), ("/nowhere/src/main.rs".into(), Some(12), Some(4)));
        assert_eq!(super::file_spec("/nowhere/a.rs:7"), ("/nowhere/a.rs".into(), Some(7), None));
        assert_eq!(super::file_spec("/nowhere/notes"), ("/nowhere/notes".into(), None, None));
        assert_eq!(super::file_spec("/nowhere/a:b.rs:3"), ("/nowhere/a:b.rs".into(), Some(3), None), "a colon in the name stays in the name");
    }
}
