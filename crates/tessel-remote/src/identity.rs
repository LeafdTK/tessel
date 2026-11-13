//! who's who: a key each side keeps, and the list of who a host lets in.
//!
//! every machine has one key (an ed25519 key in a self-signed certificate),
//! made the first time it's needed and kept under `~/.tessel/remote`. a key is
//! known by its fingerprint, `SHA256:…` as ssh writes them. a mac pins its
//! host's fingerprint (learned over ssh when it set the host up, so never on
//! trust); a host lets in the fingerprints in its `authorized` file.

use anyhow::Context;
use base64::Engine;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub struct Identity {
    pub cert: CertificateDer<'static>,
    key: Vec<u8>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self { cert: self.cert.clone(), key: self.key.clone() }
    }
}

impl Identity {
    /// this machine's key as a client (`client`) or a host (`host`): the one
    /// kept in `dir`, else a new one, kept there from now on.
    pub fn load_or_make(dir: &Path, which: &str) -> anyhow::Result<Self> {
        let (crt, key) = (dir.join(format!("{which}.crt")), dir.join(format!("{which}.key")));
        if let (Ok(c), Ok(k)) = (std::fs::read(&crt), std::fs::read(&key)) {
            return Ok(Self { cert: CertificateDer::from(c), key: k });
        }
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        let pair = rcgen::KeyPair::generate_for(&rcgen::PKCS_ED25519)?;
        let mut params = rcgen::CertificateParams::new(vec!["tessel".to_owned()])?;
        // a pinned key, not a name someone vouches for: it never needs to expire.
        params.not_before = rcgen::date_time_ymd(2025, 1, 1);
        params.not_after = rcgen::date_time_ymd(4000, 1, 1);
        let cert = params.self_signed(&pair)?;
        // the key first, so a certificate is never there without its key.
        write_private(&key, &pair.serialize_der())?;
        write_private(&crt, cert.der())?;
        Ok(Self { cert: cert.der().clone(), key: pair.serialize_der() })
    }

    pub fn key(&self) -> PrivateKeyDer<'static> {
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(self.key.clone()))
    }

    pub fn fingerprint(&self) -> String {
        fingerprint(&self.cert)
    }
}

/// `SHA256:` and the certificate's digest, unpadded base64, as ssh writes them.
pub fn fingerprint(der: &[u8]) -> String {
    let d = ring::digest::digest(&ring::digest::SHA256, der);
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(d.as_ref()))
}

/// readable by its owner only, and there whole or not at all.
fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&tmp).with_context(|| format!("writing {}", tmp.display()))?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// where a machine keeps its keys and its list: `<tessel home>/remote`.
pub fn dir(home: &Path) -> PathBuf {
    home.join("remote")
}

/// one machine a host lets in.
#[derive(Clone, Debug, PartialEq)]
pub struct Allowed {
    pub fingerprint: String,
    pub name: String,
}

fn authorized(dir: &Path) -> PathBuf {
    dir.join("authorized")
}

/// who's let in: one per line, `SHA256:… name`.
pub fn allowed(dir: &Path) -> Vec<Allowed> {
    std::fs::read_to_string(authorized(dir))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .filter_map(|l| {
            let (fp, name) = l.split_once(char::is_whitespace).unwrap_or((l, ""));
            fp.starts_with("SHA256:").then(|| Allowed { fingerprint: fp.to_owned(), name: name.trim().to_owned() })
        })
        .collect()
}

pub fn is_allowed(dir: &Path, fingerprint: &str) -> bool {
    allowed(dir).iter().any(|a| a.fingerprint == fingerprint)
}

/// let a machine in (again: its name is updated).
pub fn allow(dir: &Path, fingerprint: &str, name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(fingerprint.starts_with("SHA256:") && fingerprint.len() > 20, "{fingerprint} isn't a fingerprint (SHA256:…)");
    let mut list: Vec<Allowed> = allowed(dir).into_iter().filter(|a| a.fingerprint != fingerprint).collect();
    list.push(Allowed { fingerprint: fingerprint.to_owned(), name: name.split_whitespace().collect::<Vec<_>>().join(" ") });
    save(dir, &list)
}

/// stop letting a machine in. whether it was.
pub fn revoke(dir: &Path, fingerprint: &str) -> anyhow::Result<bool> {
    let list = allowed(dir);
    let kept: Vec<Allowed> = list.iter().filter(|a| a.fingerprint != fingerprint && a.name != fingerprint).cloned().collect();
    let gone = kept.len() != list.len();
    if gone {
        save(dir, &kept)?;
    }
    Ok(gone)
}

fn save(dir: &Path, list: &[Allowed]) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut text = String::from("# machines tessel lets in over quic: fingerprint, then a name\n");
    for a in list {
        text.push_str(&format!("{} {}\n", a.fingerprint, a.name));
    }
    write_private(&authorized(dir), text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_made_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let a = Identity::load_or_make(dir.path(), "client").unwrap();
        let b = Identity::load_or_make(dir.path(), "client").unwrap();
        assert_eq!(a.fingerprint(), b.fingerprint());
        assert!(a.fingerprint().starts_with("SHA256:"));
        let mode = std::fs::metadata(dir.path().join("client.key")).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
        assert_ne!(a.fingerprint(), Identity::load_or_make(dir.path(), "host").unwrap().fingerprint());
    }

    #[test]
    fn who_is_let_in() {
        let dir = tempfile::tempdir().unwrap();
        let fp = "SHA256:abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG";
        assert!(!is_allowed(dir.path(), fp));
        allow(dir.path(), fp, "seb's   mac").unwrap();
        allow(dir.path(), fp, "seb's mac").unwrap();
        assert_eq!(allowed(dir.path()), vec![Allowed { fingerprint: fp.into(), name: "seb's mac".into() }]);
        assert!(is_allowed(dir.path(), fp));
        assert!(revoke(dir.path(), fp).unwrap());
        assert!(!is_allowed(dir.path(), fp));
        assert!(allow(dir.path(), "nonsense", "x").is_err());
    }
}
