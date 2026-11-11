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
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
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

    let paths = Paths::from_env();
    tessel_proto::local::move_from_photon(&paths.home).map_err(anyhow::Error::msg)?;
    let mut cfg = Config::new(paths);
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
        _ => Arc::new(claude_backend()?),
    };

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async move {
        let home = cfg.paths.home.clone();
        let running = tesseld::start(cfg, backend).await?;
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
fn claude_backend() -> anyhow::Result<ClaudeBackend> {
    let extra_args: Vec<String> =
        std::env::var("TESSEL_CLAUDE_ARGS").unwrap_or_default().split_whitespace().map(str::to_owned).collect();
    if let Some(bin) = std::env::var_os("TESSEL_CLAUDE_BIN") {
        return Ok(ClaudeBackend { program: bin.into(), env: None, extra_args });
    }
    if let Some(program) = find_in_path("claude", &std::env::var("PATH").unwrap_or_default()) {
        return Ok(ClaudeBackend { program, env: None, extra_args });
    }
    let env = login_env().unwrap_or_default();
    let path = env.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()).unwrap_or_default();
    match find_in_path("claude", &path) {
        Some(program) => Ok(ClaudeBackend { program, env: Some(env), extra_args }),
        None => anyhow::bail!("could not find the `claude` cli on PATH (set TESSEL_CLAUDE_BIN to point at it)"),
    }
}

fn find_in_path(name: &str, path: &str) -> Option<PathBuf> {
    path.split(':').filter(|d| !d.is_empty()).map(|d| PathBuf::from(d).join(name)).find(|p| p.is_file())
}

fn login_env() -> Option<Vec<(String, String)>> {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
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
