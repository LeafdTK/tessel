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
use photon_proto::term::TermInfo;
use photon_proto::{ErrorCode, Response, RpcError};
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
    Capture { client: ClientId, id: u64, history: bool },
    Wait { client: ClientId, id: u64, text: Option<String>, deadline: Instant },
    /// ask the program to stop (SIGHUP to its process group).
    Hangup,
    /// stop the thread; the program is hung up first if it still runs.
    Quit,
}

/// the hub's side of a terminal.
pub struct Handle {
    pub info: TermInfo,
    pub attached: HashSet<ClientId>,
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
        .env("TERM", photon_term::TERM)
        .env("COLORTERM", "truecolor")
        .env("TERM_PROGRAM", "photon")
        .env("PHOTON_TERM", info.id.to_string())
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
    std::thread::Builder::new().name(format!("term-{id}")).spawn(move || run(thread_info, master, wake_r, child, rx, hub))?;
    Ok(Handle { info, attached: HashSet::new(), closing: false, cmds: tx, wake: wake_w })
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

/// the terminal's thread: until told to quit.
fn run(mut info: TermInfo, master: OwnedFd, wake: OwnedFd, mut child: Child, cmds: std::sync::mpsc::Receiver<Cmd>, hub: UnboundedSender<HubMsg>) {
    let fd = master.as_raw_fd();
    let Ok(mut term) = photon_term::Term::new(info.cols, info.rows, SCROLLBACK) else {
        let _ = child.kill();
        return;
    };
    // what goes to the program: typed input, and the emulator's own answers.
    let out: Rc<RefCell<VecDeque<u8>>> = Rc::default();
    let answers = out.clone();
    let _ = term.answer_with(move |b| answers.borrow_mut().extend(b.iter().copied()));

    let pid = child.id() as i32;
    let mut exited = false;
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
                Cmd::Input(bytes) => {
                    if !exited {
                        out.borrow_mut().extend(bytes);
                    }
                }
                Cmd::Resize(cols, rows) => {
                    let ws = libc::winsize { ws_row: rows.max(1), ws_col: cols.max(1), ws_xpixel: 0, ws_ypixel: 0 };
                    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
                    let _ = term.resize(cols, rows, 0, 0);
                    (info.cols, info.rows) = (cols, rows);
                }
                Cmd::Dump { client, id } => {
                    let _ = hub.send(HubMsg::TermDump { term: info.id, client, id, bytes: term.dump() });
                }
                Cmd::Capture { client, id, history } => reply(&hub, client, id, Ok(Response::TermText { text: term.text(history) })),
                Cmd::Wait { client, id, text, deadline } => waiters.push(Waiter { client, id, text, deadline }),
                Cmd::Hangup => {
                    if !exited {
                        unsafe { libc::kill(-pid, libc::SIGHUP) };
                    }
                }
                Cmd::Quit => quit = true,
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
                let status = child.wait().ok();
                let code = status.map_or(-1, |s| s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)));
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
                    None => exited,
                };
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
    if !exited {
        unsafe { libc::kill(-pid, libc::SIGHUP) };
        // reaped off this thread so a stubborn program can't hold it.
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
        });
    }
    for w in waiters {
        reply(&hub, w.client, w.id, Err(RpcError::new(ErrorCode::NotFound, format!("terminal {} was closed", info.id))));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn several_words_are_quoted_one_word_is_a_line() {
        assert_eq!(super::shell_line(&["npm run dev".into()]), "npm run dev");
        assert_eq!(super::shell_line(&["echo".into(), "it's".into()]), r#"'echo' 'it'\''s'"#);
    }
}
