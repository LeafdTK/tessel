//! where a daemon on this machine lives. not part of the wire protocol; it's
//! here because both the daemon and local clients need to agree on it.

use std::path::{Path, PathBuf};

/// `$PHOTON_HOME`, else `~/.photon`. short on purpose: macOS caps unix socket
/// paths at 104 bytes.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("PHOTON_HOME") {
        return PathBuf::from(h);
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp")).join(".photon")
}

pub fn socket(home: &Path) -> PathBuf {
    home.join("photond.sock")
}

pub fn log(home: &Path) -> PathBuf {
    home.join("photond.log")
}
