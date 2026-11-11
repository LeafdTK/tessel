//! the tessel daemon. owns sessions, agent processes and their history.
//! clients are disposable; this is not.

/// one-line descriptions of tool calls. lives in the protocol crate so clients describe tools the same way.
pub use tessel_proto::activity;
pub mod hub;
pub mod orphans;
pub mod paths;
pub mod reducer;
pub mod server;
pub mod store;
pub mod term;
pub mod workspace;
pub mod worktree;

use anyhow::Context;
use tessel_agent::Backend;
use std::os::fd::AsRawFd;
use std::sync::Arc;
use tokio::net::UnixListener;
use tokio::sync::mpsc;

/// holds the single-instance lock for as long as it lives.
pub struct InstanceLock {
    _file: std::fs::File,
}

/// take the lock that says "i am the daemon for this home". fails if another
/// daemon already holds it. the lock dies with the process, so a crashed
/// daemon never leaves a stale one behind.
pub fn lock_instance(paths: &paths::Paths) -> anyhow::Result<InstanceLock> {
    std::fs::create_dir_all(&paths.home).with_context(|| format!("creating {}", paths.home.display()))?;
    let file = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(paths.lock())?;
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    anyhow::ensure!(rc == 0, "another tesseld is already running for {}", paths.home.display());
    Ok(InstanceLock { _file: file })
}

/// a running daemon, for embedding in tests.
pub struct Running {
    pub hub: mpsc::UnboundedSender<hub::HubMsg>,
    pub done: tokio::task::JoinHandle<()>,
    _lock: InstanceLock,
}

/// start serving on the socket in `cfg.paths`. returns once it is accepting connections.
pub async fn start(cfg: hub::Config, backend: Arc<dyn Backend>) -> anyhow::Result<Running> {
    let lock = lock_instance(&cfg.paths)?;
    let socket = cfg.paths.socket();
    paths::check_socket_path(&socket)?;
    // we hold the lock, so any socket file here was left by a daemon that is gone.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;

    let store = store::Store::open(&cfg.paths.db())?;
    let (tx, rx) = mpsc::unbounded_channel();
    let hub = hub::Hub::load(cfg, store, backend, tx.clone()).await?;
    let server = tokio::spawn(server::serve(listener, tx.clone()));
    let done = tokio::spawn(async move {
        hub.run(rx).await;
        server.abort();
        let _ = std::fs::remove_file(socket);
    });
    Ok(Running { hub: tx, done, _lock: lock })
}
