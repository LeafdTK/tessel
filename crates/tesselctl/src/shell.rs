//! `tessel-shell`: the shell prefix a visual-mode agent runs every shell
//! through (`CLAUDE_CODE_SHELL_PREFIX`). it is tesselctl by another name.
//!
//! claude code runs each shell as `tessel-shell '<what to run>'`. most of
//! those (hooks, tool servers starting, the status line) are passed straight
//! on: the shell replaces this process, and nothing else happens. one of the
//! agent's own Bash commands is run here instead: it writes where it always
//! would, and what it writes is also sent, as it comes, to the session's
//! terminal in the window, where the person watches it run.
//!
//! nothing here may change what the agent gets: if the daemon can't be
//! reached, or anything else goes wrong, the command runs as if this weren't
//! here. permissions aren't touched either: they were settled on the command
//! the agent wrote before any shell started.

use std::io::{Read, Seek};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::time::{Duration, Instant};
use tessel_proto::{Request, Response, SessionId};

/// the name tesselctl answers to as a shell prefix.
pub const NAME: &str = "tessel-shell";

/// an agent's Bash command, found in what claude code asked a shell to run:
/// the command itself, and which shell its snapshot is for.
#[derive(Debug, PartialEq)]
pub struct BashCall {
    pub command: String,
    pub zsh: bool,
}

/// claude code wraps a Bash command like this (and nothing else like it):
/// `source …/shell-snapshots/snapshot-zsh-….sh … && eval '<command>' < /dev/null && pwd -P >| /tmp/claude-…-cwd`.
/// the command is the single-quoted word after `eval`, `'\''` standing for a quote.
pub fn bash_call(line: &str) -> Option<BashCall> {
    if !line.contains("pwd -P >|") {
        return None;
    }
    let at = line.find("eval '")? + "eval '".len();
    let rest = &line[at..];
    let mut command = String::new();
    let mut i = 0;
    loop {
        let end = rest[i..].find('\'')? + i;
        command.push_str(&rest[i..end]);
        if rest[end..].starts_with("'\\''") {
            command.push('\'');
            i = end + 4;
        } else {
            break;
        }
    }
    let zsh = !line.contains("snapshot-bash-");
    Some(BashCall { command, zsh })
}

/// the shell to run a line with: for a Bash command, the kind its snapshot
/// was taken in (the one claude code would have used); for the rest, sh.
fn shell_for(call: Option<&BashCall>) -> String {
    let Some(call) = call else { return "/bin/sh".into() };
    let kind = if call.zsh { "zsh" } else { "bash" };
    ["CLAUDE_CODE_SHELL", "SHELL"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|s| s.to_string_lossy().into_owned())
        .find(|s| s.rsplit('/').next() == Some(kind) && std::path::Path::new(s).exists())
        .unwrap_or_else(|| format!("/bin/{kind}"))
}

/// claude code doesn't know which shell a prefix runs, so it asks both:
/// `{ shopt -u extglob || setopt …; }`. in zsh `shopt` isn't a command, and
/// looking for it can be slow (a not-found hook, like mise's, took 2.5s).
/// knowing the shell, only its half is kept.
fn for_shell(line: &str, call: Option<&BashCall>) -> String {
    const BOTH: &str = "{ shopt -u extglob || setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; }";
    match call {
        Some(c) if line.contains(BOTH) => line.replacen(BOTH, if c.zsh { "{ setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; }" } else { "{ shopt -u extglob; }" }, 1),
        _ => line.to_owned(),
    }
}

/// what runs `line`: the person's own prefix if they had one, else a shell.
fn runner(line: &str, call: Option<&BashCall>) -> Command {
    match std::env::var("TESSEL_SHELL_PREFIX_NEXT").ok().filter(|p| !p.is_empty()) {
        Some(next) => {
            let mut c = Command::new(next);
            c.arg(line);
            c
        }
        None => {
            let mut c = Command::new(shell_for(call));
            c.arg("-c").arg(for_shell(line, call));
            c
        }
    }
}

/// run as `tessel-shell`. never returns.
pub fn main(args: Vec<String>) -> ! {
    let Some(line) = args.get(1).cloned() else {
        // nothing to run: be the shell it stands for.
        let err = Command::new("/bin/sh").args(&args[1..]).exec();
        eprintln!("{NAME}: {err}");
        std::process::exit(127);
    };
    let call = bash_call(&line);
    let session = std::env::var("TESSEL_SESSION").ok().and_then(|s| s.parse::<SessionId>().ok());
    if let (Some(c), Some(session)) = (&call, session)
        && let Some(code) = mirrored(&line, c, session)
    {
        std::process::exit(code);
    }
    let err = runner(&line, call.as_ref()).exec();
    eprintln!("{NAME}: {err}");
    std::process::exit(127);
}

/// the child, for passing signals on to.
static CHILD: AtomicI32 = AtomicI32::new(0);
static PASSED: AtomicBool = AtomicBool::new(false);

/// a signal for this process is for the command: all of it, as it would be
/// with no prefix. claude code starts each shell as the leader of a group of
/// its own, so the group is the command and whatever it started. (a signal
/// sent to the group comes back here too; it's passed on only once.)
extern "C" fn pass_on(sig: libc::c_int) {
    if PASSED.swap(true, Ordering::SeqCst) {
        return;
    }
    let leader = unsafe { libc::getpgrp() == libc::getpid() };
    let pid = CHILD.load(Ordering::Relaxed);
    unsafe {
        if leader {
            libc::kill(0, sig);
        } else if pid > 0 {
            libc::kill(pid, sig);
        }
    }
}

/// the file a descriptor writes to, if it's a plain file: where claude code
/// collects a command's output.
fn output_file(fd: i32) -> Option<(std::path::PathBuf, u64)> {
    let mut st: libc::stat = unsafe { std::mem::zeroed() };
    if unsafe { libc::fstat(fd, &mut st) } != 0 || (st.st_mode & libc::S_IFMT) != libc::S_IFREG {
        return None;
    }
    let path = path_of(fd)?;
    Some((path, st.st_size as u64))
}

#[cfg(target_os = "macos")]
fn path_of(fd: i32) -> Option<std::path::PathBuf> {
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    if unsafe { libc::fcntl(fd, libc::F_GETPATH, buf.as_mut_ptr()) } != 0 {
        return None;
    }
    let len = buf.iter().position(|b| *b == 0)?;
    Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..len])))
}

#[cfg(not(target_os = "macos"))]
fn path_of(fd: i32) -> Option<std::path::PathBuf> {
    std::fs::read_link(format!("/proc/self/fd/{fd}")).ok()
}

/// run the agent's command with its output shown in the session's terminal.
/// `None`: it couldn't be (nothing has run yet), so run it plainly.
///
/// the command writes where it always would (claude code's output file): it
/// is only read from here, through a handle of its own, as it grows. nothing
/// stands between the command and claude code, so a server it leaves running
/// in the background, or a signal, behaves exactly as without the prefix.
fn mirrored(line: &str, call: &BashCall, session: SessionId) -> Option<i32> {
    let (path, from) = output_file(1)?;
    let mut file = std::fs::File::open(&path).ok()?;
    file.seek(std::io::SeekFrom::Start(from)).ok()?;

    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().ok()?;
    let socket = tessel_proto::local::socket(&tessel_proto::local::home());
    // a daemon that's slow to answer isn't waited on: the command matters more.
    let (client, term) = rt.block_on(async {
        tokio::time::timeout(Duration::from_secs(2), async {
            let (client, _events, _) = tessel_client::Client::connect(&socket, NAME).await.ok()?;
            match client.request(Request::TermMirror { session, command: call.command.clone() }).await.ok()? {
                Response::Term { info } => Some((client, info.id)),
                _ => None,
            }
        })
        .await
        .ok()
        .flatten()
    })?;
    let client = Arc::new(client);

    let started = Instant::now();
    let mut child = runner(line, Some(call)).spawn().ok()?;
    CHILD.store(child.id() as i32, Ordering::Relaxed);
    for sig in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP, libc::SIGQUIT] {
        unsafe { libc::signal(sig, pass_on as *const () as libc::sighandler_t) };
    }

    // the output, as the file grows, until the command is done and it's all read.
    let done = Arc::new(AtomicBool::new(false));
    let follow = {
        let (client, done) = (client.clone(), done.clone());
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 64 * 1024];
            loop {
                let finished = done.load(Ordering::SeqCst);
                match file.read(&mut buf) {
                    Ok(n) if n > 0 => {
                        let _ = client.send_term_input(term, &buf[..n]);
                        continue;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    _ if finished => break,
                    // woken early once the command is done.
                    _ => std::thread::park_timeout(Duration::from_millis(20)),
                }
            }
        })
    };
    let status = loop {
        match child.wait() {
            Ok(s) => break s,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Some(1),
        }
    };
    done.store(true, Ordering::SeqCst);
    follow.thread().unpark();
    let _ = follow.join();
    let code = status.code().unwrap_or_else(|| 128 + status.signal().unwrap_or(0));

    let _ = client.send_term_input(term, footer(code, started.elapsed()).as_bytes());
    // a reply means everything sent before it has been taken in.
    rt.block_on(async {
        let _ = tokio::time::timeout(Duration::from_secs(1), client.request(Request::TermList)).await;
    });
    Some(code)
}

/// how a command ended, under its output: quietly when it went well.
fn footer(code: i32, took: Duration) -> String {
    let took = if took < Duration::from_secs(1) {
        format!("{}ms", took.as_millis())
    } else if took < Duration::from_secs(60) {
        format!("{:.1}s", took.as_secs_f64())
    } else {
        format!("{}m {}s", took.as_secs() / 60, took.as_secs() % 60)
    };
    // a line of its own, whether or not the output ended one.
    let start = "\x1b[0m\r\x1b[K";
    if code == 0 { format!("{start}\x1b[2m✓ {took}\x1b[0m\n") } else { format!("{start}\x1b[31m✗ exit {code}\x1b[0m\x1b[2m · {took}\x1b[0m\n") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bash_command_is_found_in_what_claude_code_runs() {
        let line = r#"source /Users/x/.claude/shell-snapshots/snapshot-zsh-1794527143800-k6g2ko.sh 2>/dev/null || true && { shopt -u extglob || setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; } >/dev/null 2>&1 || true && eval 'echo '\''it'\''s here'\'' && ls' < /dev/null && pwd -P >| /tmp/claude-89b0-cwd"#;
        assert_eq!(bash_call(line), Some(BashCall { command: "echo 'it's here' && ls".into(), zsh: true }));
        let bash = line.replace("snapshot-zsh-", "snapshot-bash-");
        assert!(!bash_call(&bash).unwrap().zsh);
    }

    #[test]
    fn the_shell_only_runs_the_half_meant_for_it() {
        let line = "source x/shell-snapshots/snapshot-zsh-1.sh && { shopt -u extglob || setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; } >/dev/null 2>&1 || true && eval 'ls' < /dev/null && pwd -P >| /tmp/claude-1-cwd";
        let zsh = bash_call(line);
        assert!(for_shell(line, zsh.as_ref()).contains("&& { setopt NO_EXTENDED_GLOB NO_BARE_GLOB_QUAL; } >/dev/null"));
        let bash_line = line.replace("snapshot-zsh-", "snapshot-bash-");
        assert!(for_shell(&bash_line, bash_call(&bash_line).as_ref()).contains("&& { shopt -u extglob; } >/dev/null"));
        assert_eq!(for_shell("hook.sh", None), "hook.sh", "anything else is left alone");
    }

    #[test]
    fn anything_else_is_passed_on() {
        assert_eq!(bash_call("~/.claude/hooks/notchi-hook.sh"), None);
        assert_eq!(bash_call("/usr/local/bin/some-mcp --flag 'x'"), None);
        assert_eq!(bash_call("eval 'unterminated && pwd -P >| /tmp/x"), None);
    }

    #[test]
    fn a_footer_is_quiet_when_it_went_well() {
        assert!(footer(0, Duration::from_millis(120)).contains("✓ 120ms"));
        let failed = footer(2, Duration::from_secs(75));
        assert!(failed.contains("exit 2") && failed.contains("1m 15s"));
    }
}
