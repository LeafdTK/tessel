//! tessel on other machines: a dev box's daemon, reached from the mac.
//!
//! the host runs the same tesseld as the mac, with `tesselctl remote serve`
//! in front of it: a quic service that lets in the macs it was told to and
//! relays them to the daemon's socket. the mac sets the host up over ssh
//! (`setup`), then connects over quic (or, where udp can't get through, over
//! ssh) and speaks the daemon's protocol exactly as it does locally. every
//! feature is already a daemon request, so everything works there.

pub mod forward;
pub mod hosts;
pub mod identity;
pub mod quic;
pub mod service;
pub mod setup;
pub mod ssh;

use anyhow::Context;
use hosts::{Host, Transport};
use std::path::Path;
use tessel_client::{Client, Events, Welcome};

pub use quic::Link;

/// a daemon on a host, connected to.
pub struct Connected {
    pub client: Client,
    pub events: Events,
    pub welcome: Welcome,
    /// the quic connection (none over ssh): for more streams (ports) and the round trip.
    pub link: Option<Link>,
}

/// connect to `host`'s daemon the way it was set up to be reached.
pub async fn connect(local_home: &Path, host: &Host, name: &str) -> anyhow::Result<Connected> {
    match host.transport {
        Transport::Quic => connect_quic(local_home, host, name).await,
        Transport::Ssh => {
            let stream = ssh::bridge(&ssh::To::of(host))?;
            let (client, events, welcome) =
                tokio::time::timeout(std::time::Duration::from_secs(20), Client::over(stream, name)).await.context("no answer through ssh")?.map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(Connected { client, events, welcome, link: None })
        }
    }
}

/// over quic, whatever the host is set to.
pub async fn connect_quic(local_home: &Path, host: &Host, name: &str) -> anyhow::Result<Connected> {
    let addr = host.address.as_deref().context("it hasn't been set up (no address)")?;
    let me = identity::Identity::load_or_make(&identity::dir(local_home), "client")?;
    let link = Link::connect(addr, host.fingerprint.as_deref(), &me).await?;
    let stream = link.open("tessel").await?;
    let (client, events, welcome) =
        tokio::time::timeout(std::time::Duration::from_secs(20), Client::over(stream, name)).await.context("the daemon didn't answer")?.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(Connected { client, events, welcome, link: Some(link) })
}
