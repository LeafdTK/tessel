//! where the daemon keeps things. everything hangs off one directory so a
//! test (or a second daemon) can live somewhere else entirely.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    /// where worktrees the daemon creates go.
    pub workspaces: PathBuf,
}

impl Paths {
    pub fn from_env() -> Self {
        let home = tessel_proto::local::home();
        let workspaces = std::env::var_os("TESSEL_WORKSPACES").map(PathBuf::from).unwrap_or_else(|| {
            std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp")).join("tessel").join("workspaces")
        });
        Self { home, workspaces }
    }

    pub fn at(home: impl Into<PathBuf>) -> Self {
        let home = home.into();
        let workspaces = home.join("workspaces");
        Self { home, workspaces }
    }

    pub fn socket(&self) -> PathBuf {
        tessel_proto::local::socket(&self.home)
    }

    pub fn lock(&self) -> PathBuf {
        self.home.join("tesseld.lock")
    }

    /// the daemon's settings (`DaemonSettings`), as json.
    pub fn settings(&self) -> PathBuf {
        self.home.join("settings.json")
    }

    pub fn db(&self) -> PathBuf {
        self.home.join("tessel.db")
    }

    pub fn log(&self) -> PathBuf {
        tessel_proto::local::log(&self.home)
    }

    pub fn session_dir(&self, id: &tessel_proto::SessionId) -> PathBuf {
        self.home.join("sessions").join(id.to_string())
    }

    pub fn raw_log(&self, id: &tessel_proto::SessionId) -> PathBuf {
        self.session_dir(id).join("raw.jsonl")
    }
}

/// refuse a socket path the os would silently truncate.
pub fn check_socket_path(path: &Path) -> anyhow::Result<()> {
    let len = path.as_os_str().len();
    anyhow::ensure!(len < 100, "socket path is {len} bytes, too long for a unix socket: {}", path.display());
    Ok(())
}
