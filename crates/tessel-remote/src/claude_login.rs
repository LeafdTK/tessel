//! a Claude Code token for a host, made on this mac: `claude setup-token`,
//! in a pseudo-terminal of its own. it opens the browser to sign in; the page
//! there shows a code, which is passed back to it; it answers with a token
//! that lasts a year. the host gets that token, and this mac's own sign-in is
//! never shared (two machines on one sign-in log each other out).

use anyhow::{Context, bail};
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

/// what signing in says as it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum Heard {
    /// the address to sign in at (it's opened in the browser too).
    Url(String),
    /// it's waiting for the code the browser showed.
    WantsCode,
    /// done: the token.
    Token(String),
    /// it ended without one, and why.
    Failed(String),
}

fn strip(bytes: &[u8]) -> String {
    let s = String::from_utf8_lossy(bytes);
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            match chars.peek() {
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() || c == '~' {
                            break;
                        }
                    }
                }
                Some(']') => {
                    for c in chars.by_ref() {
                        if c == '\u{7}' {
                            break;
                        }
                    }
                }
                _ => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn find_token(text: &str) -> Option<String> {
    let at = text.find("sk-ant-oat")?;
    let token: String = text[at..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    (token.len() > 40).then_some(token)
}

fn find_url(text: &str) -> Option<String> {
    let at = text.find("https://claude.")?;
    let url: String = text[at..].chars().take_while(|c| !c.is_whitespace()).collect();
    url.contains("oauth").then_some(url)
}

/// a sign-in in progress: what it says comes from `heard`; the code goes to `code`.
pub struct Login {
    pub heard: mpsc::UnboundedReceiver<Heard>,
    code: OwnedFd,
    pid: i32,
}

impl Login {
    /// pass the code the browser showed.
    pub fn code(&self, code: &str) -> anyhow::Result<()> {
        let mut f = std::fs::File::from(self.code.try_clone()?);
        f.write_all(code.trim().as_bytes())?;
        f.write_all(b"\r")?;
        Ok(())
    }
}

impl Drop for Login {
    fn drop(&mut self) {
        unsafe { libc::kill(self.pid, libc::SIGTERM) };
    }
}

/// start `claude setup-token`, in a terminal wide enough that nothing it
/// prints wraps (a wrapped token can't be read back).
pub fn start() -> anyhow::Result<Login> {
    let mut master = 0;
    let mut slave = 0;
    #[allow(unused_mut)]
    let mut size = libc::winsize { ws_row: 50, ws_col: 1000, ws_xpixel: 0, ws_ypixel: 0 };
    // macOS and linux disagree on whether these are mut; neither writes them.
    #[cfg(target_os = "macos")]
    let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null_mut(), &mut size) };
    #[cfg(not(target_os = "macos"))]
    let rc = unsafe { libc::openpty(&mut master, &mut slave, std::ptr::null_mut(), std::ptr::null(), &size) };
    anyhow::ensure!(rc == 0, "no pseudo-terminal: {}", std::io::Error::last_os_error());
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let claude = which_claude().context("Claude Code isn't installed on this Mac, so a token can't be made here")?;
    let mut cmd = std::process::Command::new(claude);
    cmd.arg("setup-token").env("TERM", "xterm-256color").stdin(slave.try_clone()?).stdout(slave.try_clone()?).stderr(slave);
    unsafe {
        std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
            libc::setsid();
            libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            Ok(())
        });
    }
    let child = cmd.spawn().context("starting claude setup-token")?;
    let pid = child.id() as i32;
    let (tx, rx) = mpsc::unbounded_channel();
    let mut reader = std::fs::File::from(master.try_clone()?);
    std::thread::Builder::new().name("tessel-claude-login".into()).spawn(move || {
        let mut child = child;
        let (mut seen, mut buf) = (Vec::new(), [0u8; 8192]);
        let (mut url_said, mut code_said) = (false, false);
        let started = Instant::now();
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            seen.extend_from_slice(&buf[..n]);
            let text = strip(&seen);
            if !url_said && let Some(u) = find_url(&text) {
                url_said = true;
                let _ = tx.send(Heard::Url(u));
            }
            if !code_said && text.contains("Paste") {
                code_said = true;
                let _ = tx.send(Heard::WantsCode);
            }
            if let Some(t) = find_token(&text) {
                let _ = tx.send(Heard::Token(t));
                let _ = child.kill();
                let _ = child.wait();
                return;
            }
            if started.elapsed() > Duration::from_secs(900) {
                break;
            }
        }
        let _ = child.kill();
        let _ = child.wait();
        let text = strip(&seen);
        let last = text.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("it stopped").to_owned();
        let _ = tx.send(Heard::Failed(last));
    })?;
    Ok(Login { heard: rx, code: master, pid })
}

/// claude, wherever the person has it (an app started from the dock has a bare PATH).
fn which_claude() -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").unwrap_or_default();
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(std::path::PathBuf::from)
        .chain([format!("{home}/.local/bin"), format!("{home}/.claude/local"), "/opt/homebrew/bin".into(), "/usr/local/bin".into()].map(std::path::PathBuf::from))
        .map(|d| d.join("claude"))
        .find(|p| p.is_file())
}

/// the whole thing, for a terminal: the address printed, the code read from stdin.
pub async fn interactive() -> anyhow::Result<String> {
    let mut login = start()?;
    while let Some(h) = login.heard.recv().await {
        match h {
            Heard::Url(u) => {
                eprintln!("sign in here (it's opening in your browser):\n  {u}");
                let _ = std::process::Command::new("open").arg(&u).status();
            }
            Heard::WantsCode => {
                eprint!("paste the code the page shows, then return: ");
                let mut code = String::new();
                std::io::stdin().read_line(&mut code)?;
                login.code(&code)?;
            }
            Heard::Token(t) => return Ok(t),
            Heard::Failed(why) => bail!("no token: {why}"),
        }
    }
    bail!("claude setup-token went away")
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_token_and_the_address_are_read_through_the_terminal_codes() {
        let raw = b"\x1b[2KYour OAuth token:\x1b[0m sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789_-ABCDEFG\x1b[0m\r\n";
        assert_eq!(super::find_token(&super::strip(raw)).as_deref(), Some("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789_-ABCDEFG"));
        let raw = b"\x1b]8;;x\x07https://claude.com/cai/oauth/authorize?code=true&x=1 \r\n";
        assert!(super::find_url(&super::strip(raw)).unwrap().starts_with("https://claude.com/cai/oauth"));
        assert_eq!(super::find_token("sk-ant-oat01-short"), None);
    }
}
