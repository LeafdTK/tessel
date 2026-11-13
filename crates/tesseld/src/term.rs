//! terminals the daemon runs.
//!
//! each terminal is a pty, the program on it (a login shell, or a command run
//! by one), and a thread that owns ghostty's emulator for it. the thread reads
//! what the program writes, keeps the screen, answers the program's questions
//! (device attributes, sizes), and passes the bytes on to the hub for whoever
//! is attached. nothing it does can block the hub: writes to the program are
//! queued and made when the pty can take them.
//!
//! terminals live as long as the daemon, not as long as a window: close the
//! window, open it again, and the shell is where you left it.

use crate::hub::{ClientId, HubMsg};
use tessel_proto::term::TermInfo;
use tessel_proto::{ErrorCode, Response, RpcError};
use std::cell::RefCell;
use std::collections::{HashSet, VecDeque};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

/// how much history each terminal keeps, in rows.
pub const SCROLLBACK: usize = 10_000;
/// what one wake-up reads at most before passing it on: big enough that a
/// flood of output is a few large messages, small enough to stay responsive.
const BATCH: usize = 256 * 1024;

pub enum Cmd {
    Input(Vec<u8>),
    Resize(u16, u16),
    /// everything on screen, for a client that is attaching.
    Dump { client: ClientId, id: u64 },
    Capture { client: ClientId, id: u64, history: bool, look: tessel_term::Look },
    /// what's running in it now.
    Process { client: ClientId, id: u64 },
    Wait { client: ClientId, id: u64, text: Option<String>, deadline: Instant },
    /// ask the program to stop (SIGHUP to its process group).
    Hangup,
    /// stop the thread; the program is hung up first if it still runs.
    Quit,
    /// stop the thread and give the terminal over, its program still running,
    /// for the next daemon to carry on with.
    Handoff(std::sync::mpsc::Sender<Released>),
}

/// a terminal let go of: its program carries on, the next daemon takes it from here.
pub struct Released {
    pub info: TermInfo,
    pub pid: i32,
    pub master: OwnedFd,
    /// everything on it and in its history, to draw it again.
    pub screen: Vec<u8>,
}

/// the hub's side of a terminal.
pub struct Handle {
    pub info: TermInfo,
    pub attached: HashSet<ClientId>,
    /// no program: it shows what `feeders` send it (an agent's own commands, mirrored).
    pub mirror: bool,
    pub feeders: HashSet<ClientId>,
    /// closed while still running: forget it as soon as it exits.
    pub closing: bool,
    cmds: std::sync::mpsc::Sender<Cmd>,
    wake: OwnedFd,
}

impl Handle {
    pub fn send(&self, cmd: Cmd) {
        if self.cmds.send(cmd).is_ok() {
            // one byte on the pipe wakes the thread's poll.
            let _ = unsafe { libc::write(self.wake.as_raw_fd(), [1u8].as_ptr().cast(), 1) };
        }
    }
}

fn last_os() -> io::Error {
    io::Error::last_os_error()
}

fn nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(last_os());
    }
    Ok(())
}

/// a command as one shell string: a single word is taken as written (so
/// `npm run dev` works), several are quoted one by one.
fn shell_line(command: &[String]) -> String {
    if command.len() == 1 {
        return command[0].clone();
    }
    command.iter().map(|a| format!("'{}'", a.replace('\'', "'\\''"))).collect::<Vec<_>>().join(" ")
}

/// start a terminal and its thread.
pub fn spawn(info: TermInfo, env: Vec<(String, String)>, hub: UnboundedSender<HubMsg>) -> io::Result<Handle> {
    let ws = libc::winsize { ws_row: info.rows.max(1), ws_col: info.cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
    let (mut master, mut user): (RawFd, RawFd) = (-1, -1);
    if unsafe { libc::openpty(&mut master, &mut user, std::ptr::null_mut(), std::ptr::null_mut(), &ws as *const _ as *mut _) } != 0 {
        return Err(last_os());
    }
    let (master, user) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(user)) };
    unsafe {
        libc::fcntl(master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }

    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let mut cmd = Command::new(&shell);
    cmd.arg("-l");
    if !info.command.is_empty() {
        cmd.arg("-c").arg(shell_line(&info.command));
    }
    cmd.current_dir(&info.cwd)
        .env("TERM", tessel_term::TERM)
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "tessel")
        .env("TESSEL_TERM", info.id.to_string())
        .envs(env)
        .stdin(Stdio::from(user.try_clone()?))
        .stdout(Stdio::from(user.try_clone()?))
        .stderr(Stdio::from(user));
    unsafe {
        cmd.pre_exec(|| {
            // its own session, with the pty as its controlling terminal, so
            // ctrl-c and job control reach it and a hangup reaches all of it.
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    nonblocking(master.as_raw_fd())?;
    let pid = child.id() as i32;
    start(info, master, Some(child), pid, Vec::new(), hub)
}

/// a terminal with no program of its own: what it shows is sent to it (as
/// input would be) by whoever runs the commands it mirrors.
pub fn mirror(info: TermInfo, hub: UnboundedSender<HubMsg>) -> io::Result<Handle> {
    let none = OwnedFd::from(std::fs::File::open("/dev/null")?);
    start(info, none, None, 0, Vec::new(), hub)
}

/// carry on with a terminal another daemon let go of.
pub fn adopt(r: Released, hub: UnboundedSender<HubMsg>) -> io::Result<Handle> {
    unsafe {
        libc::fcntl(r.master.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC);
    }
    nonblocking(r.master.as_raw_fd())?;
    start(r.info, r.master, None, r.pid, r.screen, hub)
}

/// the terminal's thread, around its pty. `child`: ours to wait on, unless
/// it was taken over. `screen`: what it showed before, to start from.
fn start(info: TermInfo, master: OwnedFd, child: Option<Child>, pid: i32, screen: Vec<u8>, hub: UnboundedSender<HubMsg>) -> io::Result<Handle> {
    let mut pipe = [0 as RawFd; 2];
    if unsafe { libc::pipe(pipe.as_mut_ptr()) } != 0 {
        return Err(last_os());
    }
    let (wake_r, wake_w) = unsafe { (OwnedFd::from_raw_fd(pipe[0]), OwnedFd::from_raw_fd(pipe[1])) };
    nonblocking(wake_r.as_raw_fd())?;
    nonblocking(wake_w.as_raw_fd())?;
    for fd in [&wake_r, &wake_w] {
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let id = info.id;
    let thread_info = info.clone();
    std::thread::Builder::new().name(format!("term-{id}")).spawn(move || run(thread_info, master, wake_r, child, pid, screen, rx, hub))?;
    Ok(Handle { info, attached: HashSet::new(), mirror: pid == 0, feeders: HashSet::new(), closing: false, cmds: tx, wake: wake_w })
}

struct Waiter {
    client: ClientId,
    id: u64,
    text: Option<String>,
    deadline: Instant,
}

fn reply(hub: &UnboundedSender<HubMsg>, client: ClientId, id: u64, result: Result<Response, RpcError>) {
    let _ = hub.send(HubMsg::Reply { client, id, result });
}

/// what a pipe wrote, for a screen: a bare line feed only moves down, so each
/// becomes a return and a line feed (a pty's line discipline does the same).
/// `after_cr`: whether the last byte before these was a return.
fn crlf(bytes: &[u8], after_cr: &mut bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + bytes.len() / 16);
    for &b in bytes {
        if b == b'\n' && !*after_cr {
            out.push(b'\r');
        }
        out.push(b);
        *after_cr = b == b'\r';
    }
    out
}

/// the terminal's thread: until told to quit.
#[allow(clippy::too_many_arguments)]
fn run(mut info: TermInfo, master: OwnedFd, wake: OwnedFd, mut child: Option<Child>, pid: i32, screen: Vec<u8>, cmds: std::sync::mpsc::Receiver<Cmd>, hub: UnboundedSender<HubMsg>) {
    let fd = master.as_raw_fd();
    // no program (pid 0): a mirror. what comes in is shown, nothing is signalled.
    let program = pid != 0;
    let mut after_cr = false;
    let Ok(mut term) = tessel_term::Term::new(info.cols, info.rows, SCROLLBACK) else {
        if let Some(c) = child.as_mut() {
            let _ = c.kill();
        }
        return;
    };
    // taken over: as it was, before anything new is written.
    if !screen.is_empty() {
        term.feed(&screen);
    }
    // what goes to the program: typed input, and the emulator's own answers.
    let out: Rc<RefCell<VecDeque<u8>>> = Rc::default();
    let answers = out.clone();
    let _ = term.answer_with(move |b| answers.borrow_mut().extend(b.iter().copied()));

    let mut exited = info.exited.is_some() || !program;
    let mut handoff = None;
    let mut waiters: Vec<Waiter> = Vec::new();
    let (mut title, mut pwd) = (String::new(), String::new());
    let mut buf = vec![0u8; 64 * 1024];
    let mut chunk: Vec<u8> = Vec::with_capacity(BATCH);

    loop {
        let timeout = waiters.iter().map(|w| w.deadline.saturating_duration_since(Instant::now())).min().map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
        let mut fds = [
            libc::pollfd { fd: wake.as_raw_fd(), events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: if exited { -1 } else { fd }, events: libc::POLLIN | if out.borrow().is_empty() { 0 } else { libc::POLLOUT }, revents: 0 },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if n < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            break;
        }

        // what the hub asked for.
        if fds[0].revents != 0 {
            let mut sink = [0u8; 64];
            while unsafe { libc::read(wake.as_raw_fd(), sink.as_mut_ptr().cast(), sink.len()) } > 0 {}
        }
        let mut quit = false;
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Input(bytes) if !program => {
                    let shown = crlf(&bytes, &mut after_cr);
                    term.feed(&shown);
                    let _ = hub.send(HubMsg::TermOutput { term: info.id, bytes: shown });
                    let (t, p) = (term.title(), term.pwd());
                    if t != title || p != pwd {
                        (title, pwd) = (t, p);
                        let _ = hub.send(HubMsg::TermChanged { term: info.id, title: title.clone(), cwd: (!pwd.is_empty()).then(|| pwd.clone()) });
                    }
                }
                Cmd::Input(bytes) => {
                    if !exited {
                        out.borrow_mut().extend(bytes);
                    }
                }
                Cmd::Resize(cols, rows) => {
                    let ws = libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
                    if program {
                        unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
                    }
                    let _ = term.resize(cols, rows, 0, 0);
                    (info.cols, info.rows) = (cols, rows);
                }
                Cmd::Dump { client, id } => {
                    let _ = hub.send(HubMsg::TermDump { term: info.id, client, id, bytes: term.dump() });
                }
                Cmd::Capture { client, id, history, look } => reply(&hub, client, id, Ok(Response::TermText { text: term.styled(history, look) })),
                Cmd::Process { client, id } => reply(&hub, client, id, Ok(if exited { Response::TermProcess { pid: None, command: String::new(), cwd: None } } else { foreground(fd) })),
                Cmd::Wait { client, id, text, deadline } => waiters.push(Waiter { client, id, text, deadline }),
                Cmd::Hangup => {
                    if program && !exited {
                        unsafe { libc::kill(-pid, libc::SIGHUP) };
                    }
                }
                Cmd::Quit => quit = true,
                Cmd::Handoff(back) => {
                    handoff = Some(back);
                    quit = true;
                }
            }
        }
        if quit {
            break;
        }

        // the program's output, as much as is there (up to a batch).
        if !exited && fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            chunk.clear();
            let mut ended = false;
            while chunk.len() < BATCH {
                let r = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
                if r > 0 {
                    chunk.extend_from_slice(&buf[..r as usize]);
                    continue;
                }
                let err = io::Error::last_os_error();
                // EIO is how a pty says the program's side closed.
                if r == 0 || err.raw_os_error() == Some(libc::EIO) {
                    ended = true;
                } else if err.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if !chunk.is_empty() {
                term.feed(&chunk);
                let _ = hub.send(HubMsg::TermOutput { term: info.id, bytes: chunk.clone() });
                let (t, p) = (term.title(), term.pwd());
                if t != title || p != pwd {
                    (title, pwd) = (t, p);
                    let _ = hub.send(HubMsg::TermChanged { term: info.id, title: title.clone(), cwd: (!pwd.is_empty()).then(|| pwd.clone()) });
                }
            }
            if ended {
                exited = true;
                // one taken over isn't ours to wait on: how it ended isn't known, so it's said plainly.
                let status = child.as_mut().and_then(|c| c.wait().ok());
                let code = match (status, &child) {
                    (Some(s), _) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                    (None, None) => 0,
                    (None, Some(_)) => -1,
                };
                info.exited = Some(code);
                let _ = hub.send(HubMsg::TermExited { term: info.id, code });
            }
        }

        // typed input and answers, as much as the pty takes now.
        if !exited && fds[1].revents & libc::POLLOUT != 0 {
            let mut q = out.borrow_mut();
            while !q.is_empty() {
                let (a, _) = q.as_slices();
                let w = unsafe { libc::write(fd, a.as_ptr().cast(), a.len()) };
                if w <= 0 {
                    break;
                }
                q.drain(..w as usize);
            }
        }

        // anyone waiting: for text on screen, or for the end.
        if !waiters.is_empty() {
            let now = Instant::now();
            let screen = waiters.iter().any(|w| w.text.is_some()).then(|| term.text(false));
            waiters.retain(|w| {
                let done = match &w.text {
                    Some(t) => screen.as_deref().is_some_and(|s| s.contains(t.as_str())),
                    None => exited && program,
                };
                // the text was awaited, and the program has ended without showing it: it never will.
                if !done && w.text.is_some() && exited && program {
                    let what = w.text.as_deref().unwrap_or_default();
                    let code = info.exited.map_or(String::new(), |c| format!(" (exit {c})"));
                    reply(&hub, w.client, w.id, Err(RpcError::new(ErrorCode::Conflict, format!("terminal {} ended{code} without showing {what:?}; it shows:\n{}", info.id, last_lines(screen.as_deref().unwrap_or_default(), 8)))));
                    return false;
                }
                if done {
                    let r = match &w.text {
                        Some(_) => Response::TermText { text: screen.clone().unwrap_or_default() },
                        None => Response::Term { info: info.clone() },
                    };
                    reply(&hub, w.client, w.id, Ok(r));
                    return false;
                }
                if now >= w.deadline {
                    let what = w.text.as_ref().map_or("to exit".to_owned(), |t| format!("to show {t:?}"));
                    reply(&hub, w.client, w.id, Err(RpcError::new(ErrorCode::Conflict, format!("timed out waiting for terminal {} {what}", info.id))));
                    return false;
                }
                true
            });
        }
    }
    // given over: the program runs on, untouched; whoever waits is told it's the next daemon's now.
    if let Some(back) = handoff {
        for w in waiters {
            reply(&hub, w.client, w.id, Err(RpcError::new(ErrorCode::NotReady, "the daemon is restarting; ask again")));
        }
        let _ = back.send(Released { screen: term.dump(), info, pid, master });
        return;
    }
    if program && !exited {
        unsafe { libc::kill(-pid, libc::SIGHUP) };
        // reaped off this thread so a stubborn program can't hold it.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            match child.as_mut() {
                Some(c) => {
                    if matches!(c.try_wait(), Ok(None)) {
                        let _ = c.kill();
                    }
                    let _ = c.wait();
                }
                None => unsafe {
                    if libc::kill(pid, 0) == 0 {
                        libc::kill(-pid, libc::SIGKILL);
                    }
                },
            }
        });
    }
    for w in waiters {
        reply(&hub, w.client, w.id, Err(RpcError::new(ErrorCode::NotFound, format!("terminal {} was closed", info.id))));
    }
}

/// the last `n` lines with anything on them, for saying what a terminal shows.
fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// the program in front on a pty: its process group's leader, what it runs, and where.
fn foreground(fd: RawFd) -> Response {
    let pid = unsafe { libc::tcgetpgrp(fd) };
    if pid <= 0 {
        return Response::TermProcess { pid: None, command: String::new(), cwd: None };
    }
    let command = std::process::Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .unwrap_or_default();
    Response::TermProcess { pid: Some(pid), command, cwd: cwd_of(pid) }
}

#[cfg(target_os = "macos")]
fn cwd_of(pid: i32) -> Option<String> {
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_vnodepathinfo>() as i32;
    let n = unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDVNODEPATHINFO, 0, (&mut info as *mut libc::proc_vnodepathinfo).cast(), size) };
    if n != size {
        return None;
    }
    let path = unsafe { std::ffi::CStr::from_ptr(info.pvi_cdir.vip_path.as_ptr().cast()) };
    Some(path.to_string_lossy().into_owned()).filter(|p| !p.is_empty())
}

#[cfg(not(target_os = "macos"))]
fn cwd_of(pid: i32) -> Option<String> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok().map(|p| p.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    #[test]
    fn several_words_are_quoted_one_word_is_a_line() {
        assert_eq!(super::shell_line(&["npm run dev".into()]), "npm run dev");
        assert_eq!(super::shell_line(&["echo".into(), "it's".into()]), r#"'echo' 'it'\''s'"#);
    }

    #[test]
    fn what_a_pipe_wrote_starts_each_line_at_the_left() {
        let mut cr = false;
        assert_eq!(super::crlf(b"a\nb\r\nc\r", &mut cr), b"a\r\nb\r\nc\r");
        // a return at the end of one write and its line feed at the start of the next are one line end.
        assert_eq!(super::crlf(b"\nd\n", &mut cr), b"\nd\r\n");
    }
}
