//! how sandboxes are reached: the settings in Settings › Hosts, kept in
//! `~/.tessel/remote.json`, and handed to a sandbox's service when it starts
//! (both ends have to agree on the relay).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// what carries a sandbox's link.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// quic by key, and websockets if that can't get through.
    #[default]
    Auto,
    /// quic by key, nothing else: fails rather than going slow.
    Quic,
    /// websockets through vercel's https route: no udp at all, no third party.
    Ws,
}

/// the relay both ends keep a link to, for the first second of a
/// connection and for when no direct path can be punched.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase", tag = "kind", content = "url")]
pub enum Relay {
    /// iroh's public relays, run by n0.
    #[default]
    N0,
    /// one of your own (`https://…`, an iroh-relay).
    Own(String),
    /// none: direct paths only (fails where the nats can't be punched).
    None,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Net {
    #[serde(default)]
    pub via: Via,
    #[serde(default)]
    pub relay: Relay,
    /// publish and look up where each key is at n0's `dns.iroh.link`.
    /// off, a sandbox is dialed at its relay (which then has to be your own).
    #[serde(default = "yes")]
    pub lookup: bool,
}

fn yes() -> bool {
    true
}

impl Default for Net {
    fn default() -> Self {
        Self { via: Via::Auto, relay: Relay::N0, lookup: true }
    }
}

pub fn path(home: &Path) -> PathBuf {
    home.join("remote.json")
}

const ENV: &str = "TESSEL_NET";

impl Net {
    pub fn load(home: &Path) -> Self {
        std::fs::read(path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        self.check()?;
        std::fs::create_dir_all(home)?;
        std::fs::write(path(home), serde_json::to_vec_pretty(self)?)?;
        Ok(())
    }

    /// what doesn't hang together.
    pub fn check(&self) -> anyhow::Result<()> {
        if let Relay::Own(url) = &self.relay {
            anyhow::ensure!(url.starts_with("https://") || url.starts_with("http://"), "a relay is an https:// address");
            url.parse::<iroh::RelayUrl>().map_err(|e| anyhow::anyhow!("{url} isn't a relay address: {e}"))?;
        }
        anyhow::ensure!(self.lookup || matches!(self.relay, Relay::Own(_)), "without the public lookup, sandboxes are found at your own relay: set one");
        Ok(())
    }

    /// as a sandbox's service is told it.
    pub fn to_env(&self) -> (String, String) {
        (ENV.to_owned(), serde_json::to_string(self).unwrap_or_default())
    }

    /// as a sandbox's service was told it (else the defaults).
    pub fn from_env() -> Self {
        std::env::var(ENV).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
    }

    /// the relay's address, when it's your own.
    pub fn relay_url(&self) -> Option<iroh::RelayUrl> {
        match &self.relay {
            Relay::Own(u) => u.parse().ok(),
            _ => None,
        }
    }

    /// an endpoint builder set up this way.
    pub(crate) fn builder(&self) -> iroh::endpoint::Builder {
        use iroh::endpoint::presets;
        let b = if self.lookup { iroh::Endpoint::builder(presets::N0) } else { iroh::Endpoint::builder(presets::Minimal) };
        match &self.relay {
            Relay::N0 => b.relay_mode(iroh::RelayMode::Default),
            Relay::Own(_) => match self.relay_url() {
                Some(u) => b.relay_mode(iroh::RelayMode::Custom(iroh::RelayMap::from(u))),
                None => b.relay_mode(iroh::RelayMode::Default),
            },
            Relay::None => b.relay_mode(iroh::RelayMode::Disabled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_keep_and_what_cant_work_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Net::load(dir.path()), Net::default());
        let own = Net { via: Via::Quic, relay: Relay::Own("https://relay.example.com".into()), lookup: false };
        own.save(dir.path()).unwrap();
        assert_eq!(Net::load(dir.path()), own);
        assert!(Net { lookup: false, ..Net::default() }.check().is_err(), "no lookup and n0's relays: nothing says where a sandbox is");
        assert!(Net { relay: Relay::Own("relay.example.com".into()), ..Net::default() }.check().is_err());
        let (k, v) = own.to_env();
        unsafe { std::env::set_var(k, v) };
        assert_eq!(Net::from_env(), own);
    }
}
