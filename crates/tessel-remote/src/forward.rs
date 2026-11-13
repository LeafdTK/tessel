//! a host's ports, here: `localhost:3000` on the mac reaching `localhost:3000`
//! on the host, so a dev server running there opens in the window's browser.
//!
//! each port is bound on this mac the first time a page asks for it: the same
//! number when it's free (so the address reads the same on both machines, and
//! a dev server's host checks pass), else any free one, and addresses are
//! rewritten on the way in and back on the way out. every connection to it is
//! a stream of its own over the host's quic link (`tcp PORT`).

use crate::quic::Link;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct State {
    /// the host's link now (a reconnect replaces it; the ports stay bound).
    link: Option<Link>,
    rt: Option<tokio::runtime::Handle>,
    /// the host's port → this mac's, and the task accepting on it.
    ports: HashMap<u16, (u16, tokio::task::AbortHandle)>,
}

#[derive(Clone, Default)]
pub struct Forwards {
    state: Arc<Mutex<State>>,
}

/// the parts of an address that matter here: whether it's this machine, and
/// its port (with where the port is written).
struct Addr {
    port: Option<(u16, usize, usize)>,
    local: bool,
}

fn parse(url: &str) -> Option<Addr> {
    let start = url.find("://").map_or(0, |i| i + 3);
    let rest = &url[start..];
    let end = start + rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let authority = &url[start..end];
    // userinfo has no business in a dev server's address.
    if authority.contains('@') {
        return None;
    }
    let (host, port) = if let Some(after) = authority.strip_prefix('[') {
        let close = after.find(']')?;
        let host = &authority[..close + 2];
        let port = authority[close + 2..].strip_prefix(':');
        (host, port)
    } else {
        match authority.rsplit_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (authority, None),
        }
    };
    let local = matches!(host.to_ascii_lowercase().as_str(), "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]" | "[::]") || host.ends_with(".localhost");
    let port = match port {
        Some(p) => Some((p.parse().ok()?, start + host.len() + 1, end)),
        None => None,
    };
    Some(Addr { port, local })
}

impl Forwards {
    /// the host the ports are reached through: a new link for the same host
    /// keeps them; `None` (this mac shown, or another host) lets them all go.
    pub fn set(&self, link: Option<Link>, rt: tokio::runtime::Handle) {
        let mut s = self.state.lock().unwrap();
        s.link = link;
        s.rt = Some(rt);
        if s.link.is_none() {
            for (_, (_, task)) in s.ports.drain() {
                task.abort();
            }
        }
    }

    /// let every port go: the window shows another machine.
    pub fn clear(&self) {
        let mut s = self.state.lock().unwrap();
        s.link = None;
        for (_, (_, task)) in s.ports.drain() {
            task.abort();
        }
    }

    /// an address as this mac reaches it: unchanged unless it's the host's
    /// own (localhost) and a host is shown.
    pub fn to_local(&self, url: &str) -> String {
        let Some(a) = parse(url).filter(|a| a.local) else { return url.to_owned() };
        let Some((port, from, to)) = a.port else { return url.to_owned() };
        match self.bind(port) {
            Some(local) if local != port => format!("{}{local}{}", &url[..from], &url[to..]),
            _ => url.to_owned(),
        }
    }

    /// an address as the host knows it: a port this mac chose instead, back to the host's.
    pub fn to_remote(&self, url: &str) -> String {
        let Some(a) = parse(url).filter(|a| a.local) else { return url.to_owned() };
        let Some((port, from, to)) = a.port else { return url.to_owned() };
        let s = self.state.lock().unwrap();
        match s.ports.iter().find(|(_, (local, _))| *local == port) {
            Some((remote, _)) if *remote != port => format!("{}{remote}{}", &url[..from], &url[to..]),
            _ => url.to_owned(),
        }
    }

    /// the ports forwarded now: the host's, and this mac's.
    pub fn ports(&self) -> Vec<(u16, u16)> {
        let mut v: Vec<(u16, u16)> = self.state.lock().unwrap().ports.iter().map(|(r, (l, _))| (*r, *l)).collect();
        v.sort();
        v
    }

    /// this mac's port for the host's `port`, bound now if it isn't yet. none
    /// when no host is shown (or nothing could be bound).
    pub fn bind(&self, port: u16) -> Option<u16> {
        let mut s = self.state.lock().unwrap();
        s.link.as_ref()?;
        if let Some((local, _)) = s.ports.get(&port) {
            return Some(*local);
        }
        let rt = s.rt.clone()?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0))).ok()?;
        listener.set_nonblocking(true).ok()?;
        let local = listener.local_addr().ok()?.port();
        let state = self.state.clone();
        let task = rt.spawn(async move {
            let Ok(listener) = tokio::net::TcpListener::from_std(listener) else { return };
            loop {
                let Ok((mut tcp, _)) = listener.accept().await else {
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    continue;
                };
                let link = state.lock().unwrap().link.clone();
                let Some(link) = link else { continue };
                tokio::spawn(async move {
                    match link.open(&format!("tcp {port}")).await {
                        Ok(mut far) => {
                            let _ = tokio::io::copy_bidirectional(&mut tcp, &mut far).await;
                        }
                        // nothing listening there (yet): the browser sees a closed connection, as it would locally.
                        Err(e) => eprintln!("tessel: forwarding :{port}: {e:#}"),
                    }
                });
            }
        });
        s.ports.insert(port, (local, task.abort_handle()));
        Some(local)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_machines_addresses_with_ports_are_forwarded() {
        let a = parse("http://localhost:3000/a?b").unwrap();
        assert!(a.local);
        assert_eq!(a.port.map(|p| p.0), Some(3000));
        assert!(parse("https://example.com:3000/").is_some_and(|a| !a.local));
        assert!(parse("http://[::1]:5173/").is_some_and(|a| a.local && a.port.unwrap().0 == 5173));
        assert!(parse("http://127.0.0.1/").is_some_and(|a| a.local && a.port.is_none()));
        assert!(parse("http://app.localhost:8080").is_some_and(|a| a.local));
        assert!(parse("http://user@localhost:1").is_none());
        // nothing shown, nothing forwarded.
        let f = Forwards::default();
        assert_eq!(f.to_local("http://localhost:3000/x"), "http://localhost:3000/x");
    }

    #[tokio::test]
    async fn a_hosts_port_opens_here_through_the_link() {
        use crate::identity::{self, Identity};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let (host, mac) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let (endpoint, host_id) = crate::quic::host_endpoint("127.0.0.1:0".parse().unwrap(), host.path()).unwrap();
        let addr = endpoint.local_addr().unwrap().to_string();
        tokio::spawn(crate::quic::serve(endpoint, host.path().to_owned()));
        // the "host's" dev server: on this machine too, so its port is taken here
        // and the forward has to pick another, and say so in the address.
        let server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = server.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut s, _) = server.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut b = [0u8; 4];
                    s.read_exact(&mut b).await.unwrap();
                    s.write_all(&b).await.unwrap();
                });
            }
        });
        let me = Identity::load_or_make(mac.path(), "client").unwrap();
        identity::allow(host.path(), &me.fingerprint(), "test").unwrap();
        let link = Link::connect(&addr, Some(&host_id.fingerprint()), &me).await.unwrap();
        let f = Forwards::default();
        f.set(Some(link), tokio::runtime::Handle::current());
        let url = f.to_local(&format!("http://localhost:{port}/app?x=1"));
        let local = f.ports()[0].1;
        assert_ne!(local, port);
        assert_eq!(url, format!("http://localhost:{local}/app?x=1"));
        assert_eq!(f.to_remote(&url), format!("http://localhost:{port}/app?x=1"), "the agent sees the host's address");
        let mut c = tokio::net::TcpStream::connect(("127.0.0.1", local)).await.unwrap();
        c.write_all(b"ping").await.unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"ping");
        f.clear();
        assert!(f.ports().is_empty());
    }
}
