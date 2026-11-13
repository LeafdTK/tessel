//! what agents left running, found two ways, folded into one row per tree:
//!
//! - lineage: everything under a running agent is its chat's. each one seen
//!   is remembered (pid and start time, so a reused pid isn't mistaken for
//!   it), so when the agent goes and they're handed to launchd they're still known.
//! - environment: what tessel gave the agent (`TESSEL_SESSION`, the workspace's
//!   `TESSEL_WORKSPACE_PATH`) is in everything it starts, and stays. read from
//!   the kernel, never guessed from a command line. macOS hides it for its own
//!   programs (sh, sleep), but not for the node, python and browsers that
//!   agents leave running; lineage covers the rest.

use std::collections::{HashMap, HashSet};
use tessel_proto::SessionId;
use tessel_proto::procs::{AgentProcess, ProcessKind};

/// one process as `ps` and the kernel describe it.
#[derive(Clone, Debug)]
struct Proc {
    pid: i32,
    ppid: i32,
    rss: u64,
    cpu: f32,
    age: u64,
    comm: String,
}

/// what tessel knows that helps: each chat's folder, and its agents' pids (not listed: they're the chats themselves).
pub struct Known {
    pub folders: Vec<(String, SessionId)>,
    pub live: HashMap<SessionId, i32>,
    /// every process seen under an agent: its chat, and when it started (seconds since the epoch).
    pub seen: HashMap<i32, (SessionId, u64)>,
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn all() -> Vec<Proc> {
    let Ok(out) = std::process::Command::new("ps").args(["-axo", "pid=,ppid=,rss=,%cpu=,etime=,comm="]).output() else { return Vec::new() };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let rss = it.next()?.parse::<u64>().ok()? * 1024;
            let cpu = it.next()?.parse().ok()?;
            let age = etime(it.next()?);
            let comm = it.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, ppid, rss, cpu, age, comm })
        })
        .collect()
}

/// `[[dd-]hh:]mm:ss` in seconds.
fn etime(s: &str) -> u64 {
    let (days, rest) = s.split_once('-').map_or((0, s), |(d, r)| (d.parse().unwrap_or(0), r));
    let parts: Vec<u64> = rest.split(':').map(|p| p.parse().unwrap_or(0)).collect();
    let hms = parts.iter().fold(0, |acc, p| acc * 60 + p);
    days * 86_400 + hms
}

/// a process's arguments and environment, as the kernel has them (only one's own processes can be read).
#[cfg(target_os = "macos")]
fn args_env(pid: i32) -> Option<(Vec<String>, Vec<String>)> {
    const KERN_PROCARGS2: libc::c_int = 49;
    let mut mib = [libc::CTL_KERN, KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, std::ptr::null_mut(), &mut size, std::ptr::null_mut(), 0) } != 0 || size < 4 {
        return None;
    }
    let mut buf = vec![0u8; size];
    if unsafe { libc::sysctl(mib.as_mut_ptr(), 3, buf.as_mut_ptr().cast(), &mut size, std::ptr::null_mut(), 0) } != 0 {
        return None;
    }
    buf.truncate(size);
    let argc = i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?) as usize;
    // the executable's path, then padding, then argc arguments, then the environment.
    let rest = &buf[4..];
    let mut words = rest.split(|b| *b == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned());
    let _exe = words.next()?;
    let args: Vec<String> = words.by_ref().take(argc).collect();
    let env: Vec<String> = words.collect();
    Some((args, env))
}

#[cfg(not(target_os = "macos"))]
fn args_env(pid: i32) -> Option<(Vec<String>, Vec<String>)> {
    let split = |b: Vec<u8>| b.split(|c| *c == 0).filter(|w| !w.is_empty()).map(|w| String::from_utf8_lossy(w).into_owned()).collect::<Vec<_>>();
    Some((split(std::fs::read(format!("/proc/{pid}/cmdline")).ok()?), split(std::fs::read(format!("/proc/{pid}/environ")).ok()?)))
}

/// the ports each process listens on.
fn listening() -> HashMap<i32, Vec<u16>> {
    let mut out: HashMap<i32, Vec<u16>> = HashMap::new();
    let Ok(o) = std::process::Command::new("lsof").args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"]).output() else { return out };
    let mut pid = 0;
    for line in String::from_utf8_lossy(&o.stdout).lines() {
        if let Some(p) = line.strip_prefix('p') {
            pid = p.parse().unwrap_or(0);
        } else if let Some(n) = line.strip_prefix('n')
            && let Some(port) = n.rsplit(':').next().and_then(|p| p.parse().ok())
        {
            let ports = out.entry(pid).or_default();
            if !ports.contains(&port) {
                ports.push(port);
            }
        }
    }
    out
}

fn is_browser(name: &str) -> bool {
    let n = name.to_lowercase();
    ["chrome", "chromium", "firefox", "headless_shell", "msedge", "webkit", "playwright"].iter().any(|b| n.contains(b))
}

fn is_shell(name: &str) -> bool {
    matches!(name.rsplit('/').next().unwrap_or(name), "sh" | "zsh" | "bash" | "fish" | "tessel-shell")
}

/// a program's name, short: its file name, or an app's name from its bundle.
fn short(comm: &str) -> String {
    if let Some(app) = comm.split(".app/").next().filter(|_| comm.contains(".app/")) {
        return app.rsplit('/').next().unwrap_or(app).to_owned();
    }
    comm.rsplit('/').next().unwrap_or(comm).to_owned()
}

/// what each process started for a chat is, one row per tree; and every
/// process now known to be a chat's, to remember for next time.
pub fn list(known: &Known) -> (Vec<AgentProcess>, HashMap<i32, (SessionId, u64)>) {
    let procs = all();
    let agents: HashSet<i32> = known.live.values().copied().collect();
    let me = std::process::id() as i32;
    // whose each is, from its environment.
    let mut ours: HashMap<i32, (Proc, Option<SessionId>, bool, String)> = HashMap::new();
    for p in &procs {
        if p.pid == me || agents.contains(&p.pid) {
            continue;
        }
        let Some((args, env)) = args_env(p.pid) else { continue };
        let var = |k: &str| env.iter().find_map(|e| e.strip_prefix(k).and_then(|v| v.strip_prefix('=')));
        let session = var("TESSEL_SESSION").and_then(|s| s.parse().ok()).or_else(|| var("TESSEL_WORKSPACE_PATH").and_then(|w| known.folders.iter().find(|(f, _)| f == w).map(|(_, s)| *s)));
        let workspace = var("TESSEL_WORKSPACE_PATH").is_some();
        if session.is_none() && !workspace {
            continue;
        }
        let command = args.join(" ");
        // tessel's own tool server: part of the agent, not something it left.
        if command.contains("tesselctl mcp") || p.comm.ends_with("tessel-voice") {
            continue;
        }
        let terminal = var("TESSEL_TERM").is_some();
        ours.insert(p.pid, (p.clone(), session, terminal, command));
    }
    // lineage: what's under a running agent is its chat's, whatever it is; and
    // what was seen under one before is still its chat's, if it's the same process.
    let now = epoch_secs();
    let by_pid: HashMap<i32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let agent_of: HashMap<i32, SessionId> = known.live.iter().map(|(s, p)| (*p, *s)).collect();
    for p in &procs {
        if ours.contains_key(&p.pid) || p.pid == me || agents.contains(&p.pid) {
            continue;
        }
        let mut at = p.ppid;
        let mut under_agent = None;
        for _ in 0..64 {
            if let Some(s) = agent_of.get(&at) {
                under_agent = Some(*s);
                break;
            }
            match by_pid.get(&at) {
                Some(q) if q.ppid > 1 && q.ppid != at => at = q.ppid,
                _ => break,
            }
        }
        let remembered = known.seen.get(&p.pid).filter(|(_, started)| now.saturating_sub(p.age).abs_diff(*started) <= 3).map(|(s, _)| *s);
        if let Some(s) = under_agent.or(remembered) {
            let command = args_env(p.pid).map_or_else(|| p.comm.clone(), |(a, _)| a.join(" "));
            if command.contains("tesselctl mcp") || p.comm.ends_with("tessel-voice") {
                continue;
            }
            ours.insert(p.pid, (p.clone(), Some(s), false, command));
        }
    }
    let seen: HashMap<i32, (SessionId, u64)> = ours.iter().filter_map(|(pid, (p, s, ..))| s.map(|s| (*pid, (s, now.saturating_sub(p.age))))).collect();
    // an agent's tool servers (its own children that aren't shells) go with it while it lives.
    let tool_of_live_agent = |p: &Proc| agents.contains(&p.ppid) && !is_shell(&p.comm);
    ours.retain(|_, (p, ..)| !tool_of_live_agent(p));

    let children_of = {
        let mut m: HashMap<i32, Vec<i32>> = HashMap::new();
        for (pid, (p, ..)) in &ours {
            m.entry(p.ppid).or_default().push(*pid);
        }
        m
    };
    let ports = listening();
    let mut rows = Vec::new();
    for (pid, (p, session, terminal, command)) in &ours {
        // a root: its parent isn't one of these.
        if ours.contains_key(&p.ppid) {
            continue;
        }
        let mut under = Vec::new();
        let mut stack = children_of.get(pid).cloned().unwrap_or_default();
        while let Some(c) = stack.pop() {
            under.push(c);
            stack.extend(children_of.get(&c).cloned().unwrap_or_default());
        }
        let tree: Vec<&Proc> = std::iter::once(p).chain(under.iter().filter_map(|c| ours.get(c).map(|o| &o.0))).collect();
        // a shell running one thing is that thing.
        let shown = if is_shell(&p.comm) { under.first().and_then(|c| ours.get(c)).map_or(p, |o| &o.0) } else { p };
        let browser = tree.iter().any(|t| is_browser(&t.comm));
        let mut all_ports: Vec<u16> = tree.iter().flat_map(|t| ports.get(&t.pid).cloned().unwrap_or_default()).collect();
        all_ports.sort_unstable();
        all_ports.dedup();
        let alive = session.is_some_and(|s| known.live.contains_key(&s));
        rows.push(AgentProcess {
            pid: *pid,
            ppid: p.ppid,
            session: *session,
            name: short(&shown.comm),
            command: if is_shell(&p.comm) { under.first().and_then(|c| ours.get(c)).map_or(command.clone(), |o| o.3.clone()) } else { command.clone() },
            kind: if *terminal {
                ProcessKind::Terminal
            } else if browser {
                ProcessKind::Browser
            } else {
                ProcessKind::Command
            },
            age_secs: p.age,
            rss_bytes: tree.iter().map(|t| t.rss).sum(),
            cpu: tree.iter().map(|t| t.cpu).sum(),
            ports: all_ports,
            children: under.len() as u32,
            // nobody will stop it: its agent is gone, or it was let go to launchd.
            left_behind: !terminal && (!alive || p.ppid == 1),
        });
    }
    rows.sort_by(|a, b| b.left_behind.cmp(&a.left_behind).then(b.rss_bytes.cmp(&a.rss_bytes)));
    (rows, seen)
}

/// stop a process tessel started, and everything under it.
pub fn stop(known: &Known, pid: i32, force: bool) -> Result<String, String> {
    let (rows, _) = list(known);
    let row = rows.iter().find(|r| r.pid == pid).ok_or_else(|| format!("{pid} isn't something an agent left running (or it has already stopped)"))?;
    let procs = all();
    let mut tree = vec![pid];
    let mut i = 0;
    while i < tree.len() {
        let parent = tree[i];
        tree.extend(procs.iter().filter(|p| p.ppid == parent).map(|p| p.pid));
        i += 1;
    }
    let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
    // the leaves first, so nothing restarts what was just stopped.
    for p in tree.iter().rev() {
        unsafe { libc::kill(*p, sig) };
    }
    Ok(format!("stopped {} ({} process{})", row.name, tree.len(), if tree.len() == 1 { "" } else { "es" }))
}

#[cfg(test)]
mod tests {
    #[test]
    fn elapsed_times_read_as_ps_writes_them() {
        assert_eq!(super::etime("05"), 5);
        assert_eq!(super::etime("01:05"), 65);
        assert_eq!(super::etime("02:01:05"), 7265);
        assert_eq!(super::etime("3-02:01:05"), 3 * 86_400 + 7265);
    }

    #[test]
    fn a_program_is_called_by_its_name() {
        assert_eq!(super::short("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"), "Google Chrome");
        assert_eq!(super::short("/usr/local/bin/node"), "node");
    }

    #[test]
    fn what_an_agent_left_running_is_found_after_it_has_gone_and_can_be_stopped() {
        let session = tessel_proto::SessionId(uuid::Uuid::new_v4());
        // an "agent", running a command in a shell of its own (as claude runs Bash), that will outlive it.
        let agent = std::process::Command::new("/bin/sh").args(["-c", "/bin/sh -c 'sleep 30; true' & wait"]).spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(300));
        let mut known = super::Known { folders: vec![], live: [(session, agent.id() as i32)].into(), seen: Default::default() };
        let (rows, seen) = super::list(&known);
        let row = rows.iter().find(|r| r.session == Some(session)).expect("what's under a running agent is its chat's");
        assert_eq!(row.name, "sleep");
        assert!(!row.left_behind, "its agent is still running");
        let pid = row.pid;

        // the agent goes; its command is handed to launchd, and still known.
        let mut agent = agent;
        let _ = agent.kill();
        let _ = agent.wait();
        std::thread::sleep(std::time::Duration::from_millis(200));
        known.live.clear();
        known.seen = seen;
        let (rows, _) = super::list(&known);
        let row = rows.iter().find(|r| r.pid == pid).expect("remembered after its agent went");
        assert!(row.left_behind, "nobody else will stop it now");
        super::stop(&known, pid, false).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(super::list(&known).0.iter().all(|r| r.pid != pid), "stopped");
        assert!(super::stop(&known, pid, false).is_err(), "and gone");
    }

    #[test]
    fn a_process_s_environment_is_read_from_the_kernel() {
        let (args, env) = super::args_env(std::process::id() as i32).expect("its own");
        assert!(!args.is_empty());
        assert!(env.iter().any(|e| e.starts_with("PATH=")));
    }
}
