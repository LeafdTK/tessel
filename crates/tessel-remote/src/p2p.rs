//! quic between keys, wherever the machines are, as tern does it (with iroh).
//!
//! each side has an ed25519 key; a host is dialed by its key, never by an
//! address. both sides keep a link to a relay, so the first packets go
//! through it at once; meanwhile they punch a direct udp path through their
//! nats and move onto it, usually within a second. after that it's plain
//! quic, mac to host: no proxy in between, no head-of-line blocking between
//! streams, and a network change on the mac moves the connection instead of
//! breaking it. where udp can't get through at all, the relay carries it.
//!
//! a host lets in the keys in its `authorized` list (as `iroh:KEY`); every
//! stream speaks what a quic stream does (see `quic`).

use crate::identity;
use iroh::endpoint::{Connection, presets};
use iroh::{Endpoint, EndpointId, SecretKey};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub const ALPN: &[u8] = b"tessel/1";

/// iroh's own runtime: an endpoint lives as long as the process, whichever
/// runtime first asked for it (a setup's runtime ends; the window's doesn't).
fn rt() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("tessel-p2p").enable_all().build().expect("a runtime"))
}

/// a key kept in `dir/{which}.iroh`, made the first time.
fn key(dir: &Path, which: &str) -> anyhow::Result<SecretKey> {
    let path = dir.join(format!("{which}.iroh"));
    if let Ok(b) = std::fs::read(&path)
        && let Ok(b) = <[u8; 32]>::try_from(b.as_slice())
    {
        return Ok(SecretKey::from_bytes(&b));
    }
    std::fs::create_dir_all(dir)?;
    let k = SecretKey::generate();
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(&k.to_bytes())?;
    f.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(k)
}

/// this machine's id as a host (`iroh:…`), as macs allow and dial it.
pub fn host_id(home: &Path) -> anyhow::Result<String> {
    Ok(format!("iroh:{}", key(&identity::dir(home), "host")?.public()))
}

/// this mac's id, as hosts let it in.
pub fn client_id(home: &Path) -> anyhow::Result<String> {
    Ok(format!("iroh:{}", key(&identity::dir(home), "client")?.public()))
}

fn parse_id(s: &str) -> anyhow::Result<EndpointId> {
    s.trim().trim_start_matches("iroh:").parse().map_err(|e| anyhow::anyhow!("{s} isn't an iroh id: {e}"))
}

// MARK: - the host

/// accept macs that are let in, and relay their streams for `home`, until killed.
pub async fn serve(home: &Path) -> anyhow::Result<()> {
    let dir = identity::dir(home);
    let secret = key(&dir, "host")?;
    let home = home.to_owned();
    // on iroh's runtime, like everything iroh: its tasks outlive nothing else then.
    rt().spawn(async move {
        let ep = Endpoint::builder(presets::N0).secret_key(secret).alpns(vec![ALPN.to_vec()]).bind().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        eprintln!("tessel remote: iroh as iroh:{}", ep.id());
        while let Some(incoming) = ep.accept().await {
            let (home, dir) = (home.clone(), dir.clone());
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(e) => {
                        eprintln!("tessel remote: iroh: {e}");
                        return;
                    }
                };
                let who = format!("iroh:{}", conn.remote_id());
                if !identity::is_allowed(&dir, &who) {
                    eprintln!("tessel remote: turned away {who}: not in {}", dir.join("authorized").display());
                    conn.close(1u32.into(), b"not allowed");
                    return;
                }
                eprintln!("tessel remote: {who} connected");
                while let Ok((send, recv)) = conn.accept_bi().await {
                    let home = home.clone();
                    tokio::spawn(async move {
                        if let Err(e) = crate::quic::relay_stream(tokio::io::join(recv, send), &home).await {
                            eprintln!("tessel remote: {e:#}");
                        }
                    });
                }
            });
        }
        anyhow::Ok(())
    })
    .await?
}

// MARK: - the mac

/// this mac's endpoint, made once and kept: its relay link and its idea of
/// its own addresses are what make a connection quick.
async fn endpoint(home: &Path) -> anyhow::Result<Endpoint> {
    static EP: tokio::sync::OnceCell<Endpoint> = tokio::sync::OnceCell::const_new();
    let secret = key(&identity::dir(home), "client")?;
    let ep = EP
        .get_or_try_init(|| async move {
            rt().spawn(async move { Endpoint::builder(presets::N0).secret_key(secret).alpns(vec![ALPN.to_vec()]).bind().await.map_err(|e| anyhow::anyhow!("{e}")) }).await?
        })
        .await?;
    Ok(ep.clone())
}

/// start getting ready (a relay, this mac's addresses) before anything is dialed.
pub fn warm(home: &Path) {
    let home = home.to_owned();
    rt().spawn(async move {
        let _ = endpoint(&home).await;
    });
}

#[derive(Clone)]
pub struct Link {
    conn: Connection,
}

impl Link {
    /// dial the host with id `id` (`iroh:…`), giving up after `within`.
    pub async fn connect(home: &Path, id: &str, within: Duration) -> anyhow::Result<Link> {
        let id = parse_id(id)?;
        let ep = endpoint(home).await?;
        let conn = rt()
            .spawn(async move { tokio::time::timeout(within, ep.connect(id, ALPN)).await })
            .await?
            .map_err(|_| anyhow::anyhow!("no answer by key within {}s", within.as_secs()))?
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(Link { conn })
    }

    /// a stream for `what` (`tessel`, or `tcp PORT`), usable at once: the
    /// host's yes (or no) is read off its front (see `link::Answered`).
    pub async fn open(&self, what: &str) -> anyhow::Result<impl crate::link::Io + use<>> {
        let (mut send, recv) = self.conn.open_bi().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        send.write_all(format!("{what}\n").as_bytes()).await?;
        Ok(crate::link::Answered::new(tokio::io::join(recv, send)))
    }

    /// the path in use: its round trip, and whether it's direct (or through the relay).
    pub fn path(&self) -> (Duration, bool) {
        let paths = self.conn.paths();
        let chosen = paths.iter().find(|p| p.is_selected());
        chosen.map_or((Duration::ZERO, false), |p| (p.rtt(), p.is_ip()))
    }

    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }
}
