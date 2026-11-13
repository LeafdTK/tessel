//! the machines this mac works on besides itself, and which one the window
//! shows: `~/.tessel/hosts.json`.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// how the mac reaches a host's daemon.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// quic, straight to the host's `tesselctl remote serve`.
    #[default]
    Quic,
    /// through ssh (`ssh HOST tesselctl bridge`): for a host whose udp port
    /// can't be reached. slower to start, and a dropped network drops it.
    Ssh,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Host {
    /// short and stable: what the window and scripts call it.
    pub id: String,
    pub name: String,
    /// how ssh reaches it: `user@host`, or a name from ~/.ssh/config.
    pub ssh: String,
    /// where its quic service is: `host:port`. learned at setup (ssh's idea of
    /// the host's address, and the port asked for).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
    /// the host's key, learned over ssh at setup. connections that show
    /// another are refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default)]
    pub transport: Transport,
    /// `x86_64` or `aarch64`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arch: Option<String>,
    /// its home directory: where projects are suggested from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    /// the tessel build it was last set up with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
}

impl Host {
    pub fn new(name: &str, ssh: &str) -> Self {
        Self {
            id: slug(if name.trim().is_empty() { ssh } else { name }),
            name: if name.trim().is_empty() { ssh.trim().to_owned() } else { name.trim().to_owned() },
            ssh: ssh.trim().to_owned(),
            address: None,
            fingerprint: None,
            transport: Transport::Quic,
            arch: None,
            home: None,
            build: None,
        }
    }

    /// set up enough to connect to.
    pub fn ready(&self) -> bool {
        match self.transport {
            Transport::Quic => self.address.is_some() && self.fingerprint.is_some(),
            Transport::Ssh => self.build.is_some(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hosts {
    #[serde(default)]
    pub hosts: Vec<Host>,
    /// the one the window shows. none: this mac.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join("hosts.json")
}

impl Hosts {
    pub fn load(home: &Path) -> Self {
        std::fs::read(path(home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self, home: &Path) -> anyhow::Result<()> {
        std::fs::create_dir_all(home)?;
        let p = path(home);
        let tmp = p.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(tmp, p)?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.id == id)
    }

    /// by id, name or ssh destination.
    pub fn find(&self, what: &str) -> Option<&Host> {
        self.get(what).or_else(|| self.hosts.iter().find(|h| h.name.eq_ignore_ascii_case(what) || h.ssh == what))
    }

    pub fn current(&self) -> Option<&Host> {
        self.current.as_deref().and_then(|id| self.get(id))
    }

    /// replace the host with this one's id (or add it).
    pub fn put(&mut self, host: Host) {
        match self.hosts.iter_mut().find(|h| h.id == host.id) {
            Some(h) => *h = host,
            None => self.hosts.push(host),
        }
    }

    /// add a new host, its id made unique. its id.
    pub fn add(&mut self, mut host: Host) -> String {
        let base = host.id.clone();
        let mut n = 2;
        while self.get(&host.id).is_some() {
            host.id = format!("{base}-{n}");
            n += 1;
        }
        let id = host.id.clone();
        self.hosts.push(host);
        id
    }

    pub fn remove(&mut self, id: &str) -> bool {
        let before = self.hosts.len();
        self.hosts.retain(|h| h.id != id);
        if self.current.as_deref() == Some(id) {
            self.current = None;
        }
        self.hosts.len() != before
    }
}

/// a name as an id: lowercase letters, digits and dashes.
pub fn slug(name: &str) -> String {
    let name = name.rsplit('@').next().unwrap_or(name);
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_end_matches('-').to_owned();
    if out.is_empty() { "host".to_owned() } else { out }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_keep_and_ids_stay_unique() {
        let dir = tempfile::tempdir().unwrap();
        let mut hosts = Hosts::load(dir.path());
        assert!(hosts.hosts.is_empty());
        let a = hosts.add(Host::new("Dev box", "seb@devbox.example.com"));
        let b = hosts.add(Host::new("dev box", "other"));
        assert_eq!((a.as_str(), b.as_str()), ("dev-box", "dev-box-2"));
        hosts.current = Some(b.clone());
        hosts.save(dir.path()).unwrap();
        let back = Hosts::load(dir.path());
        assert_eq!(back, hosts);
        assert_eq!(back.current().unwrap().ssh, "other");
        assert_eq!(back.find("seb@devbox.example.com").unwrap().id, "dev-box");
        let mut back = back;
        assert!(back.remove(&b));
        assert_eq!(back.current, None);
        assert_eq!(Host::new("", "root@1.2.3.4").id, "1-2-3-4");
    }
}
