//! tesseld: the daemon binary.
//!
//! runs in the foreground. clients that want it in the background start it
//! detached themselves; it has no daemonizing of its own.

use tessel_agent::Backend;
use tessel_agent::claude::ClaudeBackend;
use tessel_agent::replay::ReplayBackend;
use tesseld::hub::{Config, HubMsg};
use tesseld::paths::Paths;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    if let Some(a) = std::env::args().nth(1) {
        match a.as_str() {
            "--version" | "-V" => {
                println!("tesseld {} (protocol {}.{})", env!("CARGO_PKG_VERSION"), tessel_proto::PROTO_MAJOR, tessel_proto::PROTO_MINOR);
                return Ok(());
            }
            "--help" | "-h" => {
                println!("tesseld: the tessel daemon\n\n  TESSEL_HOME        state directory (default ~/.tessel)\n  TESSEL_WORKSPACES  where worktrees go (default ~/tessel/workspaces)\n  TESSEL_CLAUDE_BIN  path to the claude cli\n  TESSEL_CLAUDE_ARGS extra arguments for every claude invocation\n  TESSEL_REPLAY      comma separated recordings to replay instead of running claude\n  TESSEL_IDLE_REAP_SECS  stop idle agent processes after this long (default 900)");
                return Ok(());
            }
            other => anyhow::bail!("unknown argument {other}"),
        }
    }

    // started by a daemon handing over: take what it sends before anything else.
    let taken = match std::env::var(tesseld::handoff::ENV) {
        Ok(fd) => {
            use std::os::fd::FromRawFd;
            let fd: i32 = fd.parse()?;
            let sock = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
            let got = tesseld::handoff::recv(&sock)?;
            unsafe { std::env::remove_var(tesseld::handoff::ENV) };
            Some(got)
        }
        Err(_) => None,
    };
    let paths = Paths::from_env();
    // sign-ins a mac brought here (`tesselctl remote bring`), for what the daemon runs.
    // set before anything else runs: nothing reads the environment yet.
    for (name, file) in [("CLAUDE_CODE_OAUTH_TOKEN", "claude-token"), ("GH_TOKEN", "gh-token")] {
        if std::env::var_os(name).is_none()
            && let Ok(t) = std::fs::read_to_string(paths.home.join("remote").join(file))
            && !t.trim().is_empty()
        {
            unsafe { std::env::set_var(name, t.trim()) };
        }
    }
    tessel_proto::local::move_from_photon(&paths.home).map_err(anyhow::Error::msg)?;
    let mut cfg = Config::new(paths);
    if let Some(stamp) = std::env::current_exe().ok().and_then(|me| tessel_proto::local::binary_stamp(&me)) {
        cfg.build = format!("{} {stamp}", env!("CARGO_PKG_VERSION"));
    }
    if let Some(secs) = std::env::var("TESSEL_IDLE_REAP_SECS").ok().and_then(|s| s.parse().ok()) {
        cfg.idle_reap = Duration::from_secs(secs);
    }
    // agents get tessel's tools from the tesselctl beside us, unless told not to.
    if std::env::var("TESSEL_AGENT_TOOLS").as_deref() != Ok("0") {
        cfg.tools_bin = std::env::current_exe().ok().and_then(|me| me.parent().map(|d| d.join("tesselctl"))).filter(|p| p.is_file());
    }

    let backend: Arc<dyn Backend> = match std::env::var("TESSEL_REPLAY") {
        Ok(list) if !list.is_empty() => {
            let recs = list.split(',').map(tessel_agent::recording::load).collect::<anyhow::Result<Vec<_>>>()?;
            eprintln!("tesseld: replaying {} recording(s) instead of running claude", recs.len());
            Arc::new(ReplayBackend::new(recs).paced(Duration::from_millis(400)))
        }
        _ => Arc::new(claude_backend()),
    };

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let home = cfg.paths.home.clone();
        let running = tesseld::start_with(cfg, backend, taken).await?;
        eprintln!("tesseld: pid {} serving {}", std::process::id(), home.display());
        let hub = running.hub.clone();
        tokio::spawn(async move {
            let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).ok();
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = async { match term.as_mut() { Some(t) => { t.recv().await; } None => std::future::pending().await } } => {}
            }
            let _ = hub.send(HubMsg::Stop);
        });
        running.done.await?;
        eprintln!("tesseld: stopped");
        anyhow::Ok(())
    })
}

/// find claude, and the environment to run it in.
///
/// started from a terminal, our own environment is right. started from the
/// dock it is nearly empty and claude (often under ~/.nvm) isn't on PATH, so
/// we ask a login shell what the environment should be.
fn claude_backend() -> ClaudeBackend {
    let extra_args: Vec<String> =
        std::env::var("TESSEL_CLAUDE_ARGS").unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    if let Some(bin) = std::env::var_os("TESSEL_CLAUDE_BIN") {
        return ClaudeBackend { program: bin.into(), env: None, extra_args };
    }
    if let Some(program) = find_in_path("claude", &std::env::var("PATH").unwrap_or_default()) {
        return ClaudeBackend { program, env: None, extra_args };
    }
    let env = login_env().unwrap_or_default();
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
    if let Some(program) = find_in_path("claude", &path) {
        return ClaudeBackend { program, env: Some(env), extra_args };
    }
    // where claude's own installers put it, for a shell that never said.
    let home = std::env::var("HOME").unwrap_or_default();
    match [".local/bin/claude", ".claude/local/claude"].iter().map(|p| PathBuf::from(&home).join(p)).find(|p| p.is_file()) {
        Some(program) => ClaudeBackend { program, env: (!env.is_empty()).then_some(env), extra_args },
        // no claude: terminals, git and the rest still work; starting an agent says why it can't.
        None => {
            eprintln!("tesseld: could not find the `claude` cli on PATH (set TESSEL_CLAUDE_BIN to point at it); agents won't start until it's installed");
            ClaudeBackend { program: "claude".into(), env: (!env.is_empty()).then_some(env), extra_args }
        }
    }
}


fn find_in_path(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(name)).find(|p| p.is_file())
}

fn login_env() -> Option<Vec<(String, String)>> {
    let shell = tessel_proto::local::user_shell();
    const MARK: &str = "__tessel_env__";
    let mut child = std::process::Command::new(shell)
        .args(["-ilc", &format!("printf '{MARK}\\0'; env -0")])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    // a shell rc file that hangs must not hang the daemon.
    let (tx, rx) = std::sync::mpsc::channel();
    let mut stdout = child.stdout.take()?;
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut stdout, &mut buf);
        let _ = tx.send(buf);
    });
    let out = match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(out) => out,
        Err(_) => {
            let _ = child.kill();
            return None;
        }
    };
    let _ = child.wait();
    let text = String::from_utf8_lossy(&out);
    let after = text.split_once(&format!("{MARK}\0"))?.1;
    let env: Vec<(String, String)> =
        after.split('\0').filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
    (!env.is_empty()).then_some(env)
}
