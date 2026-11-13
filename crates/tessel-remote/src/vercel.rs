//! a dev box in a vercel sandbox: a firecracker vm that vercel keeps, named,
//! snapshotted when it stops and resumed from the snapshot when it's next
//! wanted. it has no ssh and no udp, so it's set up through vercel's api and
//! reached with websockets through its https route (see `ws`).
//!
//! it runs only while it's wanted: the window showing it keeps pushing its
//! timeout out a little at a time, and once nothing does it stops by itself
//! (and its disk is kept). showing it again resumes it.
//!
//! the api token is the person's, kept in the keychain (`tesselctl secret
//! vercel`); the team and project are in `~/.tessel/vercel.json`.

use crate::hosts::{Host, Transport};
use crate::setup::{Options, Said};
use anyhow::{Context, bail};
use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// how long a sandbox runs without being asked again, and how often it's asked.
const LEASE: Duration = Duration::from_secs(10 * 60);
const RENEW: Duration = Duration::from_secs(4 * 60);

/// a sandbox the host lives in.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sandbox {
    /// its name in the project.
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default = "two")]
    pub vcpus: u32,
    /// its own `PATH` (where its runtimes are), learned at setup: tessel's folders go in front.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

fn two() -> u32 {
    2
}

/// the account: the token, and where sandboxes go.
pub struct Account {
    token: String,
    team: String,
    project: String,
}

#[derive(serde::Deserialize, Default)]
struct Saved {
    team: Option<String>,
    project: Option<String>,
}

/// `~/.tessel/vercel.json`: the team and project sandboxes are made in.
pub fn config(home: &Path) -> PathBuf {
    home.join("vercel.json")
}

/// the token: `$VERCEL_TOKEN`, else the keychain's.
pub fn token() -> Option<String> {
    if let Ok(t) = std::env::var("VERCEL_TOKEN")
        && !t.trim().is_empty()
    {
        return Some(t.trim().to_owned());
    }
    let out = std::process::Command::new("security").args(["find-generic-password", "-s", "tessel", "-a", "vercel", "-w"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|t| !t.is_empty())
}

/// keep the token in the keychain (what Settings › Hosts does with one pasted in).
pub fn save_token(token: &str) -> anyhow::Result<()> {
    let ok = std::process::Command::new("security")
        .args(["add-generic-password", "-U", "-s", "tessel", "-a", "vercel", "-l", "tessel: vercel key", "-w", token.trim()])
        .status()?
        .success();
    anyhow::ensure!(ok, "the keychain said no");
    Ok(())
}

impl Account {
    pub fn load(home: &Path, sandbox: &Sandbox) -> anyhow::Result<Account> {
        let token = token().context("no vercel token: `tesselctl secret vercel`, or paste one in Settings › Hosts")?;
        let saved: Saved = std::fs::read(config(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let team = sandbox.team.clone().or(saved.team).or_else(|| std::env::var("VERCEL_TEAM_ID").ok()).context("which vercel team? (team in ~/.tessel/vercel.json)")?;
        let project = sandbox.project.clone().or(saved.project).or_else(|| std::env::var("VERCEL_PROJECT_ID").ok()).context("which vercel project? (project in ~/.tessel/vercel.json)")?;
        Ok(Account { token, team, project })
    }

    /// one call to the api: the token goes to curl in a file only its owner reads, never in an argument.
    async fn call(&self, method: &str, path: &str, body: Option<Vec<u8>>, content_type: &str, extra: &[(&str, &str)]) -> anyhow::Result<(u16, Vec<u8>)> {
        let dir = std::env::temp_dir().join(format!("tessel-vercel-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let cfg = dir.join(format!("{:x}.cfg", rand_u64()));
        {
            let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&cfg)?;
            writeln!(f, "header = \"Authorization: Bearer {}\"", self.token)?;
            for (k, v) in extra {
                writeln!(f, "header = \"{k}: {v}\"")?;
            }
        }
        let sep = if path.contains('?') { '&' } else { '?' };
        let url = format!("https://api.vercel.com{path}{sep}teamId={}", self.team);
        let mut cmd = tokio::process::Command::new("curl");
        cmd.args(["-sS", "-m", "900", "-K"]).arg(&cfg).args(["-X", method, "-w", "\n%{http_code}", "-H", &format!("Content-Type: {content_type}")]);
        if body.is_some() {
            cmd.args(["--data-binary", "@-"]);
        }
        cmd.arg(&url).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().context("running curl")?;
        let mut stdin = child.stdin.take().context("curl's stdin")?;
        let feed = tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Some(b) = body {
                let _ = stdin.write_all(&b).await;
            }
            let _ = stdin.shutdown().await;
        });
        let out = child.wait_with_output().await;
        let _ = std::fs::remove_file(&cfg);
        feed.abort();
        let out = out?;
        anyhow::ensure!(out.status.success(), "reaching vercel: {}", String::from_utf8_lossy(&out.stderr).trim());
        let mut text = out.stdout;
        let at = text.iter().rposition(|b| *b == b'\n').unwrap_or(0);
        let code = String::from_utf8_lossy(&text[at..]).trim().parse::<u16>().unwrap_or(0);
        text.truncate(at);
        Ok((code, text))
    }

    async fn api(&self, method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
        let body = body.map(|b| serde_json::to_vec(&b)).transpose()?.or_else(|| (method == "POST").then(|| b"{}".to_vec()));
        let (code, text) = self.call(method, path, body, "application/json", &[]).await?;
        let v: Value = serde_json::from_slice(&text).unwrap_or(Value::Null);
        if code >= 400 || code == 0 {
            let why = v["error"]["message"].as_str().or(v["message"].as_str()).map(str::to_owned).unwrap_or_else(|| String::from_utf8_lossy(&text).chars().take(300).collect());
            bail!(ApiError { code, why });
        }
        Ok(v)
    }
}

#[derive(Debug)]
struct ApiError {
    code: u16,
    why: String,
}

impl std::fmt::Display for ApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "vercel said {}: {}", self.code, self.why)
    }
}

impl std::error::Error for ApiError {}

fn rand_u64() -> u64 {
    let mut b = [0u8; 8];
    let _ = ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b);
    u64::from_le_bytes(b)
}

/// a running session of a sandbox, as the api says.
#[derive(Clone, Debug)]
pub struct Session {
    pub id: String,
    /// it had stopped, and was started from its snapshot just now: nothing runs in it yet.
    pub resumed: bool,
    /// where its websocket service is reached (its route for `ws::PORT`).
    pub url: Option<String>,
    pub arch: String,
}

fn session_of(v: &Value) -> anyhow::Result<Session> {
    let id = v["session"]["id"].as_str().context("vercel didn't say which session")?.to_owned();
    let url = v["routes"].as_array().and_then(|rs| rs.iter().find(|r| r["port"].as_u64() == Some(u64::from(crate::ws::PORT)))).and_then(|r| r["url"].as_str()).map(str::to_owned);
    let arch = v["sandbox"]["architecture"].as_str().or(v["session"]["architecture"].as_str()).unwrap_or("amd64").to_owned();
    Ok(Session { id, url, arch, resumed: v["resumed"].as_bool().unwrap_or(false) })
}

impl Account {
    /// the sandbox, running: resumed from its snapshot if it had stopped. none if there's no such sandbox.
    pub async fn wake(&self, sb: &Sandbox) -> anyhow::Result<Option<Session>> {
        match self.api("GET", &format!("/v2/sandboxes/{}?projectId={}&resume=true", sb.name, self.project), None).await {
            Ok(v) => session_of(&v).map(Some),
            Err(e) if e.downcast_ref::<ApiError>().is_some_and(|a| a.code == 404) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// a new sandbox, its websocket port open to the world (behind the token).
    pub async fn create(&self, sb: &Sandbox) -> anyhow::Result<Session> {
        let body = |timeout: u64| {
            json!({
                "name": sb.name,
                "projectId": self.project,
                "ports": [crate::ws::PORT],
                "timeout": timeout,
                "runtime": "node24",
                "resources": {"vcpus": sb.vcpus, "memory": sb.vcpus * 2048},
                "persistent": true,
            })
        };
        let v = self.api("POST", "/v2/sandboxes", Some(body(LEASE.as_millis() as u64))).await?;
        session_of(&v)
    }

    /// run `command` there and wait for it: its exit code and what it printed.
    pub async fn run(&self, session: &str, command: &str, args: &[&str], env: &[(&str, &str)]) -> anyhow::Result<(i64, String)> {
        let env: serde_json::Map<String, Value> = env.iter().map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned()))).collect();
        let body = json!({"command": command, "args": args, "env": env, "wait": true, "logs": true});
        let (code, text) = self.call("POST", &format!("/v2/sandboxes/sessions/{session}/cmd"), Some(serde_json::to_vec(&body)?), "application/json", &[]).await?;
        anyhow::ensure!(code < 400, "vercel said {code}: {}", String::from_utf8_lossy(&text).chars().take(300).collect::<String>());
        let (mut out, mut exit) = (String::new(), None);
        for line in text.split(|b| *b == b'\n').filter(|l| !l.is_empty()) {
            let Ok(v) = serde_json::from_slice::<Value>(line) else { continue };
            if let Some(d) = v["data"].as_str() {
                out.push_str(d);
            }
            if let Some(c) = v["command"]["exitCode"].as_i64() {
                exit = Some(c);
            }
        }
        Ok((exit.unwrap_or(-1), out))
    }

    /// start `command` there and leave it running.
    pub async fn start(&self, session: &str, command: &str, args: &[&str], env: &[(&str, &str)]) -> anyhow::Result<()> {
        let env: serde_json::Map<String, Value> = env.iter().map(|(k, v)| ((*k).to_owned(), Value::String((*v).to_owned()))).collect();
        self.api("POST", &format!("/v2/sandboxes/sessions/{session}/cmd"), Some(json!({"command": command, "args": args, "env": env}))).await?;
        Ok(())
    }

    /// files into `dir` there, from a gzipped tarball.
    pub async fn upload(&self, session: &str, dir: &str, tgz: Vec<u8>) -> anyhow::Result<()> {
        let (code, text) = self.call("POST", &format!("/v2/sandboxes/sessions/{session}/fs/write"), Some(tgz), "application/gzip", &[("x-cwd", dir)]).await?;
        anyhow::ensure!(code < 400, "copying files: vercel said {code}: {}", String::from_utf8_lossy(&text).chars().take(300).collect::<String>());
        Ok(())
    }

    /// a little longer.
    pub async fn extend(&self, session: &str, by: Duration) -> anyhow::Result<()> {
        self.api("POST", &format!("/v2/sandboxes/sessions/{session}/extend-timeout"), Some(json!({"duration": by.as_millis() as u64}))).await?;
        Ok(())
    }

    /// stop it now: its disk is snapshotted, for next time.
    pub async fn stop(&self, session: &str) -> anyhow::Result<()> {
        self.api("POST", &format!("/v2/sandboxes/sessions/{session}/stop"), Some(json!({}))).await?;
        Ok(())
    }
}

/// the token this mac shows a sandbox host, kept beside its keys.
fn token_path(home: &Path, id: &str) -> PathBuf {
    crate::identity::dir(home).join(format!("{id}.token"))
}

fn read_token(home: &Path, id: &str) -> anyhow::Result<String> {
    Ok(std::fs::read_to_string(token_path(home, id)).context("this mac has no token for it (set it up again)")?.trim().to_owned())
}

/// the sandbox user's home there.
const THERE: &str = "/home/vercel-sandbox";

fn service_env(home: &str, path: Option<&str>) -> Vec<(String, String)> {
    let theirs = path.unwrap_or("/vercel/runtimes/node24/bin:/usr/local/bin:/usr/bin:/bin:/usr/local/sbin:/usr/sbin");
    vec![
        ("TESSEL_HOME".into(), format!("{home}/.tessel")),
        ("PATH".into(), format!("{home}/.tessel/bin:{home}/.local/bin:{theirs}")),
        ("SHELL".into(), "/bin/bash".into()),
    ]
}

async fn start_service(acct: &Account, session: &str, home: &str, path: Option<&str>) -> anyhow::Result<()> {
    let env = service_env(home, path);
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    acct.start(session, &format!("{home}/.tessel/bin/tesselctl"), &["remote", "serve-ws", "--port", &crate::ws::PORT.to_string()], &env).await
}

/// stops pushing a sandbox's timeout out once nothing holds it.
pub struct Lease(tokio::task::AbortHandle);

impl Drop for Lease {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// the host's sandbox, running and reachable, as fast as it can be: dialed
/// by key at once (a sandbox that's running answers straight away), while
/// it's woken through the api in case it isn't. once woken, its service is
/// started if this is a fresh session, and it's dialed again. websockets,
/// through its https route, if quic by key can't get through at all. it's
/// kept running for as long as the link is held.
pub async fn connect(home: &Path, host: &Host) -> anyhow::Result<crate::Link> {
    let sb = host.vercel.clone().context("it isn't a vercel sandbox")?;
    let acct = Arc::new(Account::load(home, &sb)?);
    let token = read_token(home, &host.id)?;
    let there = host.home.clone().unwrap_or_else(|| THERE.to_owned());
    let mut woken = {
        let (acct, sb) = (acct.clone(), sb.clone());
        tokio::spawn(async move { acct.wake(&sb).await })
    };
    let dial = |within: Duration| {
        let (home, id) = (home.to_owned(), host.iroh.clone());
        async move {
            match id {
                Some(id) => crate::p2p::Link::connect(&home, &id, within).await,
                None => bail!("it has no iroh id (set it up again)"),
            }
        }
    };
    // running already: it answers by key in well under a second, and the
    // wake only matters for keeping it running, which can catch up. stopped:
    // the wake says it resumed it, and waiting on the dial is no use.
    let first = dial(Duration::from_secs(3));
    tokio::pin!(first);
    // the dial, already over (and failed): it isn't waited on again.
    let mut dialled = false;
    let woke = tokio::select! {
        r = &mut first => match r {
            Ok(l) => {
                let lease = lease(async move { Ok(woken.await??.context("the sandbox went")?.id) }, acct.clone());
                return Ok(crate::Link::P2p(l, Some(lease)));
            }
            Err(_) => {
                dialled = true;
                woken.await
            }
        },
        w = &mut woken => w,
    };
    let session = woke??.with_context(|| format!("there's no sandbox {} in the project any more (set it up again)", sb.name))?;
    // it was running all along: the dial is the way in, once it lands.
    if !session.resumed
        && !dialled
        && let Ok(l) = first.await
    {
        let id = session.id.clone();
        return Ok(crate::Link::P2p(l, Some(lease(async move { Ok(id) }, acct))));
    }
    let url = session.url.clone().context("the sandbox has no route for tessel's port")?;
    // woken just now (or its service had stopped): nothing listens yet.
    if session.resumed || crate::ws::Link::connect(&url, &token).await.is_err() {
        start_service(&acct, &session.id, &there, sb.path.as_deref()).await?;
    }
    let id = session.id.clone();
    let lease = lease(async move { Ok(id) }, acct);
    match dial(Duration::from_secs(12)).await {
        Ok(l) => Ok(crate::Link::P2p(l, Some(lease))),
        Err(e) => {
            eprintln!("tessel: quic by key didn't get through ({e:#}); using websockets");
            let deadline = tokio::time::Instant::now() + Duration::from_secs(25);
            loop {
                match crate::ws::Link::connect(&url, &token).await {
                    Ok(l) => break Ok(crate::Link::Ws(l.held_by_arc(lease))),
                    Err(e) if tokio::time::Instant::now() >= deadline => return Err(e.context("tessel's service there didn't come up")),
                    Err(_) => tokio::time::sleep(Duration::from_millis(500)).await,
                }
            }
        }
    }
}

/// keep a sandbox running while this is held: its timeout pushed out now,
/// and again every few minutes. `session` says which, when it knows.
fn lease(session: impl std::future::Future<Output = anyhow::Result<String>> + Send + 'static, acct: Arc<Account>) -> Arc<Lease> {
    let task = tokio::spawn(async move {
        let id = match session.await {
            Ok(id) => id,
            Err(e) => {
                eprintln!("tessel: keeping the sandbox running: {e:#}");
                return;
            }
        };
        loop {
            if let Err(e) = acct.extend(&id, LEASE).await {
                eprintln!("tessel: keeping the sandbox running: {e:#}");
            }
            tokio::time::sleep(RENEW).await;
        }
    });
    Arc::new(Lease(task.abort_handle()))
}

/// stop the host's sandbox now (it would by itself, soon after nothing shows it).
pub async fn stop(home: &Path, host: &Host) -> anyhow::Result<bool> {
    let sb = host.vercel.clone().context("it isn't a vercel sandbox")?;
    let acct = Account::load(home, &sb)?;
    let v = acct.api("GET", &format!("/v2/sandboxes/{}?projectId={}", sb.name, acct.project), None).await?;
    if v["sandbox"]["status"].as_str() != Some("running") {
        return Ok(false);
    }
    let id = v["session"]["id"].as_str().context("no session")?;
    acct.stop(id).await?;
    Ok(true)
}

/// set a sandbox host up: the sandbox made (or woken), tessel copied in, this
/// mac's token put there, claude installed, the service started, connected.
pub async fn setup(home: &Path, host: &mut Host, opts: &Options, say: &mut (dyn FnMut(Said) + Send)) -> anyhow::Result<()> {
    let sb = host.vercel.clone().context("it isn't a vercel sandbox")?;
    let acct = Account::load(home, &sb)?;
    say(Said::Step(format!("Starting the sandbox {}", sb.name)));
    let session = match acct.wake(&sb).await? {
        Some(s) => {
            say(Said::Note("it was there already: woken from its snapshot".into()));
            s
        }
        None => acct.create(&sb).await?,
    };
    let url = session.url.clone().context("the sandbox has no route for tessel's port")?;
    let (code, out) = acct.run(&session.id, "sh", &["-c", "printf '%s\\n' \"$HOME\"; if command -v claude >/dev/null 2>&1 || [ -x \"$HOME/.local/bin/claude\" ]; then echo claude; else echo -; fi; printf '%s\\n' \"$PATH\"; mkdir -p \"$HOME/.tessel/bin\""], &[]).await?;
    anyhow::ensure!(code == 0, "couldn't look around in the sandbox: {out}");
    let lines: Vec<&str> = out.lines().map(str::trim).collect();
    let there = lines.first().copied().filter(|h| h.starts_with('/')).unwrap_or(THERE).to_owned();
    let has_claude = lines.get(1) == Some(&"claude");
    // its own PATH, without tessel's folders (they go in front).
    let theirs: Vec<&str> = lines.get(2).copied().unwrap_or("").split(':').filter(|d| !d.is_empty() && !d.contains("/.tessel/") && !d.ends_with("/.local/bin")).collect();
    let mut sb = sb;
    sb.path = (!theirs.is_empty()).then(|| theirs.join(":"));
    host.vercel = Some(sb.clone());
    let arch = if session.arch == "arm64" { "aarch64" } else { "x86_64" };
    host.arch = Some(arch.to_owned());
    host.home = Some(there.clone());

    say(Said::Step(format!("Copying tessel in (linux {arch})")));
    let bins = crate::setup::binaries("linux", arch)?;
    let dir = bins[0].parent().context("no folder")?.to_owned();
    let tar = tokio::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg("-").arg("-C").arg(&dir).args(["tesseld", "tesselctl"]).output().await?;
    anyhow::ensure!(tar.status.success(), "packing tessel: {}", String::from_utf8_lossy(&tar.stderr));
    say(Said::Note(format!("{:.1} MB", tar.stdout.len() as f64 / 1_048_576.0)));
    acct.upload(&session.id, &format!("{there}/.tessel/bin"), tar.stdout).await?;

    say(Said::Step("Letting this Mac in".into()));
    let token = crate::ws::new_token()?;
    let (code, out) = acct
        .run(&session.id, "sh", &["-c", "umask 077; mkdir -p \"$HOME/.tessel/remote\" && printf %s \"$TESSEL_TOKEN\" > \"$HOME/.tessel/remote/ws-token\" && chmod 755 \"$HOME/.tessel/bin/tesseld\" \"$HOME/.tessel/bin/tesselctl\""], &[("TESSEL_TOKEN", &token)])
        .await?;
    anyhow::ensure!(code == 0, "putting the token there: {out}");
    std::fs::create_dir_all(crate::identity::dir(home))?;
    {
        let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(token_path(home, &host.id))?;
        f.write_all(token.as_bytes())?;
    }
    // and by key, for quic through the nats: its id, and this mac's let in.
    let env = service_env(&there, sb.path.as_deref());
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let me = crate::p2p::client_id(home)?;
    let (code, out) = acct.run(&session.id, "sh", &["-c", "tesselctl remote allow \"$MAC\" \"$NAME\" >/dev/null && tesselctl remote iroh-id"], &[env.as_slice(), &[("MAC", me.as_str()), ("NAME", crate::setup::machine_name().as_str())]].concat()).await?;
    anyhow::ensure!(code == 0, "letting this mac in by key: {out}");
    host.iroh = out.lines().map(str::trim).find(|l| l.starts_with("iroh:")).map(str::to_owned);

    if !has_claude && opts.claude {
        say(Said::Step("Installing Claude Code".into()));
        let (code, out) = acct.run(&session.id, "sh", &["-c", "curl -fsSL https://claude.ai/install.sh | bash"], &[]).await?;
        if code != 0 {
            say(Said::Note(format!("couldn't install it ({}); install claude there yourself", out.trim().lines().last().unwrap_or("it failed"))));
        }
    }

    say(Said::Step("Starting tessel's service".into()));
    // a service from an earlier setup holds the port (this one has the new
    // token), and a daemon from then is older than what was just copied in:
    // it hands over, its terminals and agents carrying on.
    let env = service_env(&there, sb.path.as_deref());
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let _ = acct
        .run(&session.id, "sh", &["-c", "pkill -f 'tesselctl remote serve-ws'; if [ -S \"$TESSEL_HOME/tesseld.sock\" ]; then tesselctl daemon restart >/dev/null 2>&1; fi; true"], &env)
        .await;
    start_service(&acct, &session.id, &there, sb.path.as_deref()).await?;

    say(Said::Step("Connecting".into()));
    host.address = Some(url);
    host.transport = Transport::Vercel;
    let c = crate::connect(home, host, "tessel-setup").await?;
    host.build = Some(c.welcome.build.clone());
    Ok(())
}
