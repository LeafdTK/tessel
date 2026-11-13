//! the host's side: the quic service kept running (a systemd user service
//! where there is systemd, else a background process), and the ssh bridge.

use crate::identity::{self, Identity};
use anyhow::Context;
use std::path::Path;
use std::time::Duration;

/// listen on `port` (every address, v6 and v4) and relay for `home` until killed.
pub async fn serve(home: &Path, port: u16) -> anyhow::Result<()> {
    let dir = identity::dir(home);
    // both families where the kernel does v6; v4 alone where it doesn't.
    let (endpoint, id) = match crate::quic::host_endpoint(format!("[::]:{port}").parse()?, &dir) {
        Ok(e) => e,
        Err(_) => crate::quic::host_endpoint(format!("0.0.0.0:{port}").parse()?, &dir)?,
    };
    eprintln!("tessel remote: listening on udp {} as {}, for {}", endpoint.local_addr()?, id.fingerprint(), home.display());
    crate::quic::serve(endpoint, home.to_owned()).await;
    Ok(())
}

/// stdin and stdout, joined to the daemon's socket (started if need be): how
/// a mac reaches this host over ssh.
pub async fn bridge(home: &Path) -> anyhow::Result<()> {
    let socket = tessel_proto::local::socket(home);
    let mut daemon = match tokio::net::UnixStream::connect(&socket).await {
        Ok(s) => s,
        Err(_) => {
            tessel_client::start_daemon(home).map_err(|e| anyhow::anyhow!("{e}"))?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            loop {
                match tokio::net::UnixStream::connect(&socket).await {
                    Ok(s) => break s,
                    Err(e) if tokio::time::Instant::now() >= deadline => return Err(e).context("the daemon didn't come up"),
                    Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
                }
            }
        }
    };
    let mut io = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    let _ = tokio::io::copy_bidirectional(&mut io, &mut daemon).await;
    Ok(())
}

const UNIT: &str = "tessel-remote.service";

/// keep `tesselctl remote serve --port PORT` running from now on, and through
/// reboots. prints what it did; ends with the host's fingerprint.
pub fn install(home: &Path, port: u16) -> anyhow::Result<String> {
    let me = std::env::current_exe()?;
    let id = Identity::load_or_make(&identity::dir(home), "host")?;
    let mut said = Vec::new();
    let systemd = std::process::Command::new("systemctl").args(["--user", "show-environment"]).output().is_ok_and(|o| o.status.success());
    if systemd {
        let user_home = std::env::var("HOME").context("no $HOME")?;
        let dir = Path::new(&user_home).join(".config/systemd/user");
        std::fs::create_dir_all(&dir)?;
        let bin = me.parent().map(|d| d.display().to_string()).unwrap_or_default();
        // KillMode=process: the daemon it starts lives in this service's
        // group, and must outlive a restart of the service.
        let unit = format!(
            "[Unit]\nDescription=tessel: the quic service in front of tesseld\nAfter=network-online.target\n\n\
             [Service]\nExecStart={} remote serve --port {port}\nEnvironment=TESSEL_HOME={}\n\
             Environment=PATH={bin}:{user_home}/.local/bin:{user_home}/.npm-global/bin:{user_home}/.bun/bin:/usr/local/bin:/usr/bin:/bin\n\
             Restart=always\nRestartSec=2\nKillMode=process\n\n[Install]\nWantedBy=default.target\n",
            me.display(),
            home.display()
        );
        std::fs::write(dir.join(UNIT), unit)?;
        let sh = |args: &[&str]| std::process::Command::new("systemctl").arg("--user").args(args).output();
        sh(&["daemon-reload"])?;
        let up = sh(&["enable", UNIT]).and_then(|_| sh(&["restart", UNIT]))?;
        anyhow::ensure!(up.status.success(), "systemctl --user restart {UNIT}: {}", String::from_utf8_lossy(&up.stderr).trim());
        said.push(format!("running as the systemd user service {UNIT}"));
        // without lingering, user services stop at logout and don't start at boot.
        let user = std::env::var("USER").unwrap_or_default();
        let linger = std::process::Command::new("loginctl").args(["enable-linger", &user]).output();
        if !linger.is_ok_and(|o| o.status.success()) {
            said.push(format!("couldn't keep it running after logout (`sudo loginctl enable-linger {user}` does)"));
        }
    } else {
        // no systemd: a process of its own, the one before it stopped (by the pid it left).
        let pidfile = identity::dir(home).join("serve.pid");
        if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<i32>().ok()) {
            unsafe { libc::kill(pid, libc::SIGTERM) };
            std::thread::sleep(Duration::from_millis(300));
        }
        let log = std::fs::OpenOptions::new().create(true).append(true).open(home.join("remote.log"))?;
        let mut cmd = std::process::Command::new(&me);
        cmd.args(["remote", "serve", "--port", &port.to_string()]).env("TESSEL_HOME", home).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, || {
                libc::setsid();
                Ok(())
            });
        }
        let child = cmd.spawn()?;
        std::fs::write(&pidfile, child.id().to_string())?;
        said.push(format!("running in the background (pid {}); without systemd it won't come back after a reboot", child.id()));
    }
    said.push(format!("fingerprint {}", id.fingerprint()));
    Ok(said.join("\n"))
}
