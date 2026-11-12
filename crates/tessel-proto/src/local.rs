//! where a daemon on this machine lives. not part of the wire protocol; it's
//! here because both the daemon and local clients need to agree on it.

use std::path::{Path, PathBuf};

/// `$TESSEL_HOME`, else `~/.tessel`. short on purpose: macOS caps unix socket
/// paths at 104 bytes.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("TESSEL_HOME") {
        return PathBuf::from(h);
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp")).join(".tessel")
}

pub fn socket(home: &Path) -> PathBuf {
    home.join("tesseld.sock")
}

pub fn log(home: &Path) -> PathBuf {
    home.join("tesseld.log")
}

/// a home from before tessel was called tessel (`~/.photon`) becomes this one,
/// with the files in it renamed to match, the first time a daemon starts here.
/// never while the old daemon still runs: its database is open.
pub fn move_from_photon(home: &Path) -> Result<(), String> {
    // only the default home: one asked for by name is left as it is.
    let default = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("/tmp")).join(".tessel");
    if home != default || home.exists() {
        return Ok(());
    }
    let Some(old) = home.parent().map(|p| p.join(".photon")) else { return Ok(()) };
    if !old.is_dir() {
        return Ok(());
    }
    move_home(&old, home)
}

/// move `old` (a photon home) to `new`, renaming what's inside.
pub fn move_home(old: &Path, new: &Path) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    let lock = old.join("photond.lock");
    if let Ok(f) = std::fs::OpenOptions::new().read(true).write(true).open(&lock) {
        // the old daemon holds this while it runs.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(format!(
                "photon (tessel's old name) is still running for {}. quit it (or run `photonctl daemon stop`), then start tessel again: your sessions come with you",
                old.display()
            ));
        }
    }
    std::fs::rename(old, new).map_err(|e| format!("moving {} to {}: {e}", old.display(), new.display()))?;
    for (from, to) in [("photon.db", "tessel.db"), ("photon.db-wal", "tessel.db-wal"), ("photon.db-shm", "tessel.db-shm"), ("photond.lock", "tesseld.lock"), ("photond.log", "tesseld.log")] {
        let from = new.join(from);
        if from.exists() {
            std::fs::rename(&from, new.join(to)).map_err(|e| format!("renaming {}: {e}", from.display()))?;
        }
    }
    // the old socket is dead: the new daemon makes its own.
    let _ = std::fs::remove_file(new.join("photond.sock"));
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_photon_home_moves_with_its_files_renamed_unless_its_daemon_runs() {
        let root = std::env::temp_dir().join(format!("tessel-move-{}", std::process::id()));
        let (old, new) = (root.join(".photon"), root.join(".tessel"));
        std::fs::create_dir_all(old.join("sessions/s1")).unwrap();
        for f in ["photon.db", "photon.db-wal", "photond.lock", "photond.sock", "ui.json"] {
            std::fs::write(old.join(f), f).unwrap();
        }
        std::fs::write(old.join("sessions/s1/raw.jsonl"), "x").unwrap();

        // the old daemon is running: nothing moves.
        {
            use std::os::fd::AsRawFd;
            let held = std::fs::OpenOptions::new().read(true).write(true).open(old.join("photond.lock")).unwrap();
            assert_eq!(unsafe { libc::flock(held.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) }, 0);
            let err = super::move_home(&old, &new).unwrap_err().to_string();
            assert!(err.contains("still running"), "{err}");
            assert!(old.exists() && !new.exists());
        }

        // once it has quit, it moves. (another test's child process can share
        // the lock for the moment between its fork and exec: try a few times,
        // as someone starting tessel just after quitting photon would.)
        let mut moved = super::move_home(&old, &new);
        for _ in 0..50 {
            if moved.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            moved = super::move_home(&old, &new);
        }
        moved.unwrap();
        assert!(!old.exists());
        assert_eq!(std::fs::read_to_string(new.join("tessel.db")).unwrap(), "photon.db");
        assert_eq!(std::fs::read_to_string(new.join("tessel.db-wal")).unwrap(), "photon.db-wal");
        assert!(new.join("tesseld.lock").exists() && !new.join("photond.sock").exists());
        assert_eq!(std::fs::read_to_string(new.join("ui.json")).unwrap(), "ui.json", "the rest as it was");
        assert!(new.join("sessions/s1/raw.jsonl").exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// which build of a binary is on disk: when it was written. a daemon says
/// its own in its welcome (`build`, after the version), so a client can tell
/// the daemon on disk is newer than the one running.
pub fn binary_stamp(path: &Path) -> Option<String> {
    let t = std::fs::metadata(path).ok()?.modified().ok()?;
    let d = t.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(format!("{}.{:09}", d.as_secs(), d.subsec_nanos()))
}

/// the stamp in a daemon's `build`, if it gave one.
pub fn build_stamp(build: &str) -> Option<&str> {
    build.split_whitespace().nth(1)
}
