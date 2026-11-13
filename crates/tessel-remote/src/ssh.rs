//! ssh, as the person has it set up: their keys, their agent, their
//! ~/.ssh/config. used to set a host up, and to reach one whose udp port is closed.

use anyhow::{Context, bail};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// the ssh command: `$TESSEL_SSH` (e.g. `ssh -F other_config`), else `ssh`.
fn command() -> tokio::process::Command {
    let custom = std::env::var("TESSEL_SSH").unwrap_or_default();
    let mut words = custom.split_whitespace();
    let mut cmd = tokio::process::Command::new(words.next().unwrap_or("ssh"));
    cmd.args(words);
    // never a password prompt: there's no terminal to type it into.
    cmd.args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=5"]);
    cmd.kill_on_drop(true);
    cmd
}

pub struct Ran {
    pub ok: bool,
    pub out: String,
    pub err: String,
}

/// run `script` (posix sh) on `dest`. it's sent on stdin, so the login shell
/// there (bash, zsh, fish) doesn't matter.
pub async fn run(dest: &str, script: &str, timeout: Duration) -> anyhow::Result<Ran> {
    run_with(dest, "sh -s", script.as_bytes(), timeout).await
}

/// run `command` on `dest`, with `input` on its stdin.
pub async fn run_with(dest: &str, command_line: &str, input: &[u8], timeout: Duration) -> anyhow::Result<Ran> {
    let mut cmd = command();
    cmd.args(["-T", "--", dest, command_line]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().context("running ssh")?;
    let mut stdin = child.stdin.take().context("ssh's stdin")?;
    let input = input.to_vec();
    let feed = tokio::spawn(async move {
        let _ = stdin.write_all(&input).await;
        let _ = stdin.shutdown().await;
    });
    let (mut so, mut se) = (child.stdout.take().context("ssh's stdout")?, child.stderr.take().context("ssh's stderr")?);
    let work = async {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let _ = tokio::join!(so.read_to_end(&mut out), se.read_to_end(&mut err));
        let status = child.wait().await?;
        anyhow::Ok((status, out, err))
    };
    let (status, out, err) = match tokio::time::timeout(timeout, work).await {
        Ok(r) => r?,
        Err(_) => bail!("ssh {dest} took longer than {}s", timeout.as_secs()),
    };
    feed.abort();
    let err = String::from_utf8_lossy(&err).into_owned();
    // 255: ssh itself failed (couldn't connect, or wasn't let in).
    if status.code() == Some(255) {
        bail!("ssh {dest}: {}", reach_error(&err));
    }
    Ok(Ran { ok: status.success(), out: String::from_utf8_lossy(&out).into_owned(), err })
}

/// ssh's complaint, said so it's clear what to do.
fn reach_error(err: &str) -> String {
    let last = err.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with("Warning:")).last().unwrap_or("it couldn't connect").to_owned();
    if last.contains("Permission denied") {
        format!("{last}. tessel uses your ssh keys (and agent) with no password prompt: `ssh-copy-id` your key there first")
    } else if last.contains("Host key verification failed") {
        format!("{last}. connect once with `ssh` in a terminal to check and accept the host's key")
    } else {
        last
    }
}

/// what ssh will actually connect to for `dest` (after ~/.ssh/config):
/// its hostname, and its user.
pub async fn resolve(dest: &str) -> anyhow::Result<(String, String)> {
    let mut cmd = command();
    cmd.args(["-G", "--", dest]).stdin(Stdio::null());
    let out = tokio::time::timeout(Duration::from_secs(5), cmd.output()).await.context("ssh -G took too long")??;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |k: &str| text.lines().find_map(|l| l.strip_prefix(k).and_then(|v| v.strip_prefix(' ')).map(str::to_owned));
    let host = field("hostname").with_context(|| format!("ssh doesn't know how to reach {dest}"))?;
    Ok((host, field("user").unwrap_or_default()))
}

/// a stream to the host's daemon, through ssh: `tesselctl bridge` there.
pub fn bridge(dest: &str) -> anyhow::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static> {
    let mut cmd = command();
    cmd.args(["-T", "--", dest, "\"$HOME/.tessel/bin/tesselctl\" bridge"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = cmd.spawn().context("running ssh")?;
    let (stdout, stdin) = (child.stdout.take().context("ssh's stdout")?, child.stdin.take().context("ssh's stdin")?);
    Ok(Piped { io: tokio::io::join(stdout, stdin), _child: child })
}

/// a process's stdout and stdin as one stream, and the process with it.
struct Piped {
    io: tokio::io::Join<tokio::process::ChildStdout, tokio::process::ChildStdin>,
    _child: tokio::process::Child,
}

impl tokio::io::AsyncRead for Piped {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl tokio::io::AsyncWrite for Piped {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.io).poll_write(cx, buf)
    }
    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

/// a word for a posix shell, quoted.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    #[test]
    fn quoting_survives_quotes() {
        assert_eq!(super::quote("seb's mac"), r"'seb'\''s mac'");
    }
}
