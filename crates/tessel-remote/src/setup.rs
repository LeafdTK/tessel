//! setting a host up from the mac, over ssh: tessel copied there, this mac
//! let in, the quic service started, claude installed, and a connection
//! tried. run again, it updates: only what changed is copied.

use crate::hosts::{Host, Transport};
use crate::identity::{self, Identity};
use crate::ssh::{self, quote};
use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// what setting up says as it goes.
#[derive(Clone, Debug, PartialEq)]
pub enum Said {
    /// a step started (the one before it is done).
    Step(String),
    /// something worth knowing, about the step under way.
    Note(String),
}

pub struct Options {
    pub port: u16,
    /// install claude code there if it isn't.
    pub claude: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { port: crate::quic::DEFAULT_PORT, claude: true }
    }
}

const PROBE: &str = r#"uname -sm
printf '%s\n' "$HOME"
if command -v claude >/dev/null 2>&1 || [ -x "$HOME/.local/bin/claude" ] || [ -x "$HOME/.claude/local/claude" ]; then echo claude; else echo -; fi
for b in tesseld tesselctl; do (sha256sum "$HOME/.tessel/bin/$b" 2>/dev/null || shasum -a 256 "$HOME/.tessel/bin/$b" 2>/dev/null || echo "- $b") | cut -d' ' -f1; done
"#;

/// set `host` up (or bring it up to date). `host` is filled in as it goes:
/// its address, key, transport and build.
pub async fn setup(local_home: &Path, host: &mut Host, opts: &Options, say: &mut (dyn FnMut(Said) + Send)) -> anyhow::Result<()> {
    if host.vercel.is_some() {
        return crate::vercel::setup(local_home, host, opts, say).await;
    }
    let dest = ssh::To::of(host);
    say(Said::Step(format!("Reaching {} over ssh", dest.dest)));
    let (hostname, _user) = ssh::resolve(&dest).await?;
    let probe = ssh::run(&dest, PROBE, Duration::from_secs(30)).await?;
    anyhow::ensure!(probe.ok, "couldn't look around on {}: {}", dest.dest, probe.err.trim());
    let lines: Vec<&str> = probe.out.lines().map(str::trim).collect();
    anyhow::ensure!(lines.len() >= 5, "{} answered oddly: {:?}", dest.dest, probe.out);
    let (os, arch) = platform(lines[0])?;
    host.arch = Some(arch.to_owned());
    host.home = Some(lines[1].to_owned());
    let has_claude = lines[2] == "claude";
    let there = [lines[3], lines[4]];

    say(Said::Step(format!("Copying tessel to {} ({os} {arch})", host.name)));
    let bins = binaries(os, arch)?;
    let mut copied = false;
    for (i, name) in ["tesseld", "tesselctl"].iter().enumerate() {
        let path = &bins[i];
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let sum = hex(ring::digest::digest(&ring::digest::SHA256, &bytes).as_ref());
        if there[i] == sum {
            continue;
        }
        say(Said::Note(format!("{name}, {:.1} MB", bytes.len() as f64 / 1_048_576.0)));
        let script = format!(
            "sh -c 'mkdir -p \"$HOME/.tessel/bin\" && cat > \"$HOME/.tessel/bin/.{name}.new\" && chmod 755 \"$HOME/.tessel/bin/.{name}.new\" && mv -f \"$HOME/.tessel/bin/.{name}.new\" \"$HOME/.tessel/bin/{name}\"'"
        );
        let r = ssh::run_with(&dest, &script, &bytes, Duration::from_secs(600)).await?;
        anyhow::ensure!(r.ok, "copying {name}: {}", r.err.trim());
        copied = true;
    }
    if !copied {
        say(Said::Note("already up to date".into()));
    }

    say(Said::Step("Letting this Mac in".into()));
    let me = Identity::load_or_make(&identity::dir(local_home), "client")?;
    let r = ssh::run(&dest, &format!("\"$HOME/.tessel/bin/tesselctl\" remote allow {} {}\n", quote(&me.fingerprint()), quote(&machine_name())), Duration::from_secs(30)).await?;
    anyhow::ensure!(r.ok, "letting this mac in: {}", r.err.trim());

    say(Said::Step("Starting tessel's service".into()));
    // a newer daemon takes over from the one running (terminals and agents carry on).
    let restart = if copied { "if [ -S \"$HOME/.tessel/tesseld.sock\" ]; then \"$HOME/.tessel/bin/tesselctl\" daemon restart >/dev/null 2>&1 || true; fi\n" } else { "" };
    let r = ssh::run(&dest, &format!("{restart}\"$HOME/.tessel/bin/tesselctl\" remote install --port {}\n", opts.port), Duration::from_secs(60)).await?;
    anyhow::ensure!(r.ok, "starting the service: {}{}", r.err.trim(), r.out.trim());
    let mut fingerprint = None;
    for line in r.out.lines() {
        match line.strip_prefix("fingerprint ") {
            Some(fp) => fingerprint = Some(fp.trim().to_owned()),
            None if !line.trim().is_empty() && !line.starts_with("running as") => say(Said::Note(line.trim().to_owned())),
            None => {}
        }
    }
    host.fingerprint = Some(fingerprint.context("the service didn't say its key")?);
    host.address = Some(crate::quic::with_port(&format!("{}:{}", bracket(&hostname), opts.port)));

    if !has_claude && opts.claude {
        say(Said::Step("Installing Claude Code".into()));
        let r = ssh::run(&dest, "curl -fsSL https://claude.ai/install.sh | bash\n", Duration::from_secs(600)).await?;
        if !r.ok {
            say(Said::Note(format!("couldn't install it ({}); install claude there yourself", r.err.trim().lines().last().unwrap_or("it failed"))));
        }
    }

    say(Said::Step("Connecting over QUIC".into()));
    match crate::connect_quic(local_home, host, "tessel-setup").await {
        Ok(c) => {
            host.transport = Transport::Quic;
            host.build = Some(c.welcome.build.clone());
        }
        Err(e) => {
            say(Said::Note(format!("{e:#}. Using ssh until udp port {} is open: open it in the host's firewall for quic.", opts.port)));
            say(Said::Step("Connecting over ssh".into()));
            host.transport = Transport::Ssh;
            let c = crate::connect(local_home, host, "tessel-setup").await.context("over ssh")?;
            host.build = Some(c.welcome.build.clone());
        }
    }
    Ok(())
}

fn platform(uname: &str) -> anyhow::Result<(&'static str, &'static str)> {
    let mut words = uname.split_whitespace();
    let os = match words.next() {
        Some("Linux") => "linux",
        Some("Darwin") => "macos",
        other => bail!("tessel runs on linux (and macs); this is {}", other.unwrap_or("something else")),
    };
    let arch = match words.next() {
        Some("x86_64" | "amd64") => "x86_64",
        Some("aarch64" | "arm64") => "aarch64",
        other => bail!("tessel runs on x86_64 and arm64; this is {}", other.unwrap_or("something else")),
    };
    Ok((os, arch))
}

/// the daemon and tesselctl built for a host: shipped in the app
/// (`Resources/linux/ARCH`), in `$TESSEL_LINUX_BINS/ARCH`, or (building tessel
/// itself) in the workspace's cross-built target.
pub fn binaries(os: &str, arch: &str) -> anyhow::Result<[PathBuf; 2]> {
    let me = std::env::current_exe()?;
    let here = me.parent().map(Path::to_path_buf).unwrap_or_default();
    let mut dirs: Vec<PathBuf> = Vec::new();
    if os == "macos" {
        anyhow::ensure!(arch == std::env::consts::ARCH, "this mac's tessel is for {}; the host is {arch}", std::env::consts::ARCH);
        dirs.push(here.clone());
    } else {
        if let Some(d) = std::env::var_os("TESSEL_LINUX_BINS") {
            dirs.push(PathBuf::from(d).join(arch));
        }
        dirs.push(here.join("../Resources/linux").join(arch));
        let triple = format!("{arch}-unknown-linux-musl");
        for up in here.ancestors() {
            if up.file_name().is_some_and(|n| n == "target") {
                dirs.push(up.join(&triple).join("release"));
            }
        }
    }
    for d in &dirs {
        let pair = [d.join("tesseld"), d.join("tesselctl")];
        if pair.iter().all(|p| p.is_file()) {
            return Ok(pair);
        }
    }
    bail!("this tessel has no {os} {arch} build to copy over (scripts/build-linux.sh makes one); looked in {}", dirs.iter().map(|d| d.display().to_string()).collect::<Vec<_>>().join(", "))
}

/// what this mac is called, for the host's list of who it lets in.
pub fn machine_name() -> String {
    let name = std::process::Command::new("scutil").args(["--get", "ComputerName"]).output().ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    name.filter(|n| !n.is_empty()).unwrap_or_else(|| {
        let mut buf = [0u8; 256];
        unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
        String::from_utf8_lossy(&buf).trim_end_matches('\0').to_owned()
    })
}

fn bracket(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') { format!("[{host}]") } else { host.to_owned() }
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platforms_are_read() {
        assert_eq!(platform("Linux x86_64").unwrap(), ("linux", "x86_64"));
        assert_eq!(platform("Linux aarch64").unwrap(), ("linux", "aarch64"));
        assert_eq!(platform("Darwin arm64").unwrap(), ("macos", "aarch64"));
        assert!(platform("FreeBSD amd64").is_err());
        assert!(platform("Linux riscv64").is_err());
        assert_eq!(bracket("::1"), "[::1]");
        assert_eq!(bracket("devbox"), "devbox");
    }
}
