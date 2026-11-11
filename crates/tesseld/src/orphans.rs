//! agent processes that outlived a daemon.
//!
//! a daemon that is killed outright can't stop its agents on the way out, so
//! they keep running with nobody watching: burning tokens, holding files,
//! finishing commands no one will see. each agent's pid is written beside its
//! session; the next daemon to start stops whatever is still there.
//!
//! a pid alone is not proof (the os reuses them), so before stopping anything
//! we check the process's command line names this session.

use std::path::{Path, PathBuf};

fn pid_file(session_dir: &Path) -> PathBuf {
    session_dir.join("agent.pid")
}

pub fn record(session_dir: &Path, pid: u32) {
    let _ = std::fs::create_dir_all(session_dir);
    let _ = std::fs::write(pid_file(session_dir), pid.to_string());
}

pub fn clear(session_dir: &Path) {
    let _ = std::fs::remove_file(pid_file(session_dir));
}

/// stop the agent recorded for this session if it is still running and its
/// command line contains one of `markers`. returns the pid that was stopped.
pub fn reap(session_dir: &Path, markers: &[&str]) -> Option<u32> {
    let file = pid_file(session_dir);
    let pid: u32 = std::fs::read_to_string(&file).ok()?.trim().parse().ok()?;
    let _ = std::fs::remove_file(&file);
    if pid <= 1 {
        return None;
    }
    let command = command_line(pid)?;
    if !markers.iter().any(|m| !m.is_empty() && command.contains(m)) {
        // that pid belongs to something else now. leave it alone.
        return None;
    }
    // ask first. an agent runs its tools in process groups of their own, so
    // killing it outright would orphan whatever command it had running; given
    // the chance, it stops them itself.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + GRACE;
    while std::time::Instant::now() < deadline && command_line(pid).is_some() {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    // then insist. harmless if it already went.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
    Some(pid)
}

/// how long a leftover agent gets to shut down cleanly before it is killed.
const GRACE: std::time::Duration = std::time::Duration::from_secs(3);

fn command_line(pid: u32) -> Option<String> {
    let out = std::process::Command::new("ps").args(["-o", "command=", "-p", &pid.to_string()]).output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::CommandExt;

    fn alive(pid: u32) -> bool {
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }

    fn wait_gone(child: &mut std::process::Child) -> bool {
        for _ in 0..200 {
            if child.try_wait().ok().flatten().is_some() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_leftover_agent_and_its_children_are_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let marker = uuid::Uuid::new_v4().to_string();
        // a group leader with a child, like an agent running a tool.
        let mut child = std::process::Command::new("sh")
            .args(["-c", &format!("sleep 600 & echo $! > {}/kid; wait # {marker}", dir.path().display())])
            .process_group(0)
            .spawn()
            .unwrap();
        let kid: u32 = loop {
            if let Ok(s) = std::fs::read_to_string(dir.path().join("kid"))
                && let Ok(p) = s.trim().parse()
            {
                break p;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        };
        record(dir.path(), child.id());
        assert!(alive(child.id()) && alive(kid));

        assert_eq!(reap(dir.path(), &["nope", &marker]), Some(child.id()));
        assert!(wait_gone(&mut child), "the agent was not stopped");
        for _ in 0..200 {
            if !alive(kid) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!alive(kid), "its child was left running");
        assert!(!pid_file(dir.path()).exists());
        assert_eq!(reap(dir.path(), &[&marker]), None, "nothing recorded, nothing to do");
    }

    #[test]
    fn a_reused_pid_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        // some unrelated process now has the recorded pid.
        let mut other = std::process::Command::new("sleep").arg("600").process_group(0).spawn().unwrap();
        record(dir.path(), other.id());
        assert_eq!(reap(dir.path(), &["a-session-id-that-is-not-in-its-command-line", ""]), None);
        assert!(alive(other.id()), "killed a process that was not ours");
        assert!(!pid_file(dir.path()).exists(), "the stale record is still dropped");
        other.kill().unwrap();
        other.wait().unwrap();
    }

    #[test]
    fn garbage_and_dead_pids_are_ignored() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(reap(dir.path(), &["x"]), None);
        std::fs::write(pid_file(dir.path()), "not a pid").unwrap();
        assert_eq!(reap(dir.path(), &["x"]), None);
        std::fs::write(pid_file(dir.path()), "1").unwrap();
        assert_eq!(reap(dir.path(), &["launchd", "init", "x"]), None, "never pid 1");
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        gone.wait().unwrap();
        record(dir.path(), gone.id());
        assert_eq!(reap(dir.path(), &["true"]), None);
    }
}
