//! the link between a mac and a host: one quic connection, a stream per use.
//!
//! on the host, `tesselctl remote serve` listens on udp and lets in the
//! machines in its `authorized` list (each proves its key in the handshake).
//! every stream starts with one line saying what it's for:
//!
//!   `tessel`     the daemon's own protocol, relayed to its socket untouched
//!   `tcp PORT`   a port on the host (a dev server, for the browser)
//!
//! and the host answers `ok`, or `no WHY`. then the bytes are the stream's.
//! the relay never reads the daemon's protocol, so a daemon handing over to
//! a newer one is only a reconnect, like on the mac.
//!
//! quic, not tcp: a laptop that changes networks keeps its connection, and a
//! terminal pouring out output doesn't hold up anything else on another stream.

use crate::identity::{self, Identity};
use anyhow::{Context, bail};
use quinn::{Connection, Endpoint, RecvStream, SendStream};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context as TaskCx, Poll};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader, ReadBuf};

pub const ALPN: &[u8] = b"tessel/1";
/// where a host listens unless told otherwise (udp).
pub const DEFAULT_PORT: u16 = 7731;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut t = quinn::TransportConfig::default();
    // often enough that a nat keeps the mapping, and that a dead link is noticed soon.
    t.keep_alive_interval(Some(Duration::from_secs(4)));
    t.max_idle_timeout(Some(Duration::from_secs(20).try_into().expect("idle timeout")));
    Arc::new(t)
}

// MARK: - the host

/// lets in a client whose key is in the host's list, read at every handshake
/// (so `allow` and `revoke` take effect without a restart).
#[derive(Debug)]
struct Authorized {
    dir: PathBuf,
    provider: Arc<CryptoProvider>,
}

impl ClientCertVerifier for Authorized {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        let fp = identity::fingerprint(end_entity);
        if identity::is_allowed(&self.dir, &fp) {
            Ok(ClientCertVerified::assertion())
        } else {
            eprintln!("tessel remote: turned away {fp}: not in {}", self.dir.join("authorized").display());
            Err(rustls::Error::General(format!("{fp} isn't allowed on this host")))
        }
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// the host's endpoint: its key, listening on `listen`.
pub fn host_endpoint(listen: SocketAddr, dir: &Path) -> anyhow::Result<(Endpoint, Identity)> {
    let id = Identity::load_or_make(dir, "host")?;
    let provider = provider();
    let mut tls = rustls::ServerConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(Arc::new(Authorized { dir: dir.to_owned(), provider }))
        .with_single_cert(vec![id.cert.clone()], id.key())?;
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let mut server = quinn::ServerConfig::with_crypto(Arc::new(quinn::crypto::rustls::QuicServerConfig::try_from(tls)?));
    server.transport_config(transport());
    let endpoint = Endpoint::server(server, listen).with_context(|| format!("listening on udp {listen}"))?;
    Ok((endpoint, id))
}

/// serve until the endpoint closes: every stream relayed to the daemon for
/// `home` (started if it isn't running) or to a port.
pub async fn serve(endpoint: Endpoint, home: PathBuf) {
    while let Some(incoming) = endpoint.accept().await {
        let home = home.clone();
        tokio::spawn(async move {
            let from = incoming.remote_address();
            let conn = match incoming.await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("tessel remote: {from}: {e}");
                    return;
                }
            };
            let who = peer(&conn).unwrap_or_default();
            eprintln!("tessel remote: {from} connected ({who})");
            loop {
                match conn.accept_bi().await {
                    Ok((send, recv)) => {
                        let home = home.clone();
                        tokio::spawn(async move {
                            if let Err(e) = relay(send, recv, &home).await {
                                eprintln!("tessel remote: {from}: {e:#}");
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("tessel remote: {from} gone ({e})");
                        return;
                    }
                }
            }
        });
    }
}

/// the fingerprint of whoever's on the other end.
fn peer(conn: &Connection) -> Option<String> {
    let certs = conn.peer_identity()?.downcast::<Vec<CertificateDer<'static>>>().ok()?;
    certs.first().map(|c| identity::fingerprint(c))
}

async fn relay(send: SendStream, recv: RecvStream, home: &Path) -> anyhow::Result<()> {
    relay_stream(tokio::io::join(recv, send), home).await
}

/// one stream from a mac, whatever carries it (a quic stream, a websocket):
/// its first line says what it's for, and the rest is relayed there.
pub(crate) async fn relay_stream(stream: impl AsyncRead + AsyncWrite + Unpin + Send, home: &Path) -> anyhow::Result<()> {
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    // a line, not a novel: whoever sends one isn't speaking this.
    let n = tokio::time::timeout(Duration::from_secs(10), (&mut stream).take(256).read_line(&mut line)).await.context("no header")??;
    anyhow::ensure!(n > 0, "closed before saying what for");
    let what = line.trim();
    let target: Result<Box<dyn Duplex>, String> = match what.split_once(' ') {
        None if what == "tessel" => daemon(home).await.map(|s| Box::new(s) as Box<dyn Duplex>),
        Some(("tcp", port)) => match port.parse::<u16>() {
            Ok(port) => local_port(port).await.map(|s| Box::new(s) as Box<dyn Duplex>),
            Err(_) => Err(format!("{port} isn't a port")),
        },
        _ => Err(format!("this host doesn't do {what:?}")),
    };
    let mut target = match target {
        Ok(t) => t,
        Err(why) => {
            stream.write_all(format!("no {why}\n").as_bytes()).await?;
            let _ = stream.shutdown().await;
            return Ok(());
        }
    };
    stream.write_all(b"ok\n").await?;
    stream.flush().await?;
    let _ = tokio::io::copy_bidirectional(&mut stream, &mut target).await;
    Ok(())
}

trait Duplex: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Duplex for T {}

/// the daemon's socket, starting the daemon if nothing answers.
async fn daemon(home: &Path) -> Result<tokio::net::UnixStream, String> {
    let socket = tessel_proto::local::socket(home);
    if let Ok(s) = tokio::net::UnixStream::connect(&socket).await {
        return Ok(s);
    }
    tessel_client::start_daemon(home).map_err(|e| e.to_string())?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match tokio::net::UnixStream::connect(&socket).await {
            Ok(s) => return Ok(s),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                return Err(format!("the daemon didn't come up ({e}); see {}", tessel_proto::local::log(home).display()));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

async fn local_port(port: u16) -> Result<tokio::net::TcpStream, String> {
    // a dev server listens on one or the other, rarely both.
    match tokio::net::TcpStream::connect(("127.0.0.1", port)).await {
        Ok(s) => Ok(s),
        Err(_) => tokio::net::TcpStream::connect(("::1", port)).await.map_err(|e| format!("nothing on port {port} ({e})")),
    }
}

// MARK: - the mac

/// a host's key, as the mac knows it: pinned, or (the first time, when it
/// wasn't learned over ssh) whatever answers, noted down.
#[derive(Debug)]
struct Pinned {
    want: Option<String>,
    seen: Mutex<Option<String>>,
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        let fp = identity::fingerprint(end_entity);
        *self.seen.lock().unwrap() = Some(fp.clone());
        match &self.want {
            Some(want) if *want != fp => Err(rustls::Error::General(format!("the host's key is {fp}, not the {want} it was set up with"))),
            _ => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// a connection to a host. cheap to clone; streams are opened on it as needed.
#[derive(Clone)]
pub struct Link {
    conn: Connection,
    // the endpoint's driver runs as long as someone holds it.
    _endpoint: Endpoint,
    /// the host's fingerprint, as it showed it.
    pub fingerprint: String,
}

/// `host`, `host:port`, `[v6]:port` or `v6`: with the default port when none is said.
pub fn with_port(addr: &str) -> String {
    let addr = addr.trim();
    if addr.parse::<SocketAddr>().is_ok() {
        return addr.to_owned();
    }
    if addr.parse::<std::net::Ipv6Addr>().is_ok() {
        return format!("[{addr}]:{DEFAULT_PORT}");
    }
    match addr.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => addr.to_owned(),
        _ => format!("{addr}:{DEFAULT_PORT}"),
    }
}

impl Link {
    /// connect to the host at `addr`, as `me`, expecting its key to be `pinned`.
    pub async fn connect(addr: &str, pinned: Option<&str>, me: &Identity) -> anyhow::Result<Link> {
        let addr = with_port(addr);
        let to = tokio::net::lookup_host(&addr).await.with_context(|| format!("looking up {addr}"))?.next().with_context(|| format!("{addr} has no address"))?;
        let provider = provider();
        let pinned = Arc::new(Pinned { want: pinned.map(str::to_owned), seen: Mutex::new(None), provider: provider.clone() });
        let mut tls = rustls::ClientConfig::builder_with_provider(provider)
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(pinned.clone())
            .with_client_auth_cert(vec![me.cert.clone()], me.key())?;
        tls.alpn_protocols = vec![ALPN.to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(quinn::crypto::rustls::QuicClientConfig::try_from(tls)?));
        config.transport_config(transport());
        let bind: SocketAddr = if to.is_ipv6() { "[::]:0".parse()? } else { "0.0.0.0:0".parse()? };
        let mut endpoint = Endpoint::client(bind)?;
        endpoint.set_default_client_config(config);
        let connecting = endpoint.connect(to, "tessel")?;
        let conn = match tokio::time::timeout(Duration::from_secs(6), connecting).await {
            Err(_) | Ok(Err(quinn::ConnectionError::TimedOut)) => {
                bail!("no answer from {addr} over udp (is tessel's service running there, and udp port {} open?)", to.port())
            }
            Ok(Err(e)) => bail!("{addr}: {e}"),
            Ok(Ok(c)) => c,
        };
        let fingerprint = pinned.seen.lock().unwrap().clone().unwrap_or_default();
        Ok(Link { conn, _endpoint: endpoint, fingerprint })
    }

    /// a stream for `what` (`tessel`, or `tcp PORT`), once the host says yes.
    pub async fn open(&self, what: &str) -> anyhow::Result<QuicStream> {
        let (mut send, recv) = self.conn.open_bi().await?;
        send.write_all(format!("{what}\n").as_bytes()).await?;
        let mut recv = BufReader::new(recv);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), (&mut recv).take(1024).read_line(&mut line)).await.context("the host didn't answer")??;
        match line.trim() {
            "ok" => Ok(QuicStream { io: tokio::io::join(recv, send), _conn: self.conn.clone() }),
            "" => bail!("the host closed the stream"),
            other => bail!("{}", other.strip_prefix("no ").unwrap_or(other)),
        }
    }

    pub fn rtt(&self) -> Duration {
        self.conn.rtt()
    }

    pub fn is_closed(&self) -> bool {
        self.conn.close_reason().is_some()
    }

    pub fn close(&self) {
        self.conn.close(0u32.into(), b"bye");
    }
}

/// one stream to a host: reads and writes like a socket.
pub struct QuicStream {
    io: tokio::io::Join<BufReader<RecvStream>, SendStream>,
    _conn: Connection,
}

impl AsyncRead for QuicStream {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.io).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut TaskCx<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn addresses_get_the_default_port() {
        assert_eq!(with_port("devbox.example.com"), format!("devbox.example.com:{DEFAULT_PORT}"));
        assert_eq!(with_port("1.2.3.4:9000"), "1.2.3.4:9000");
        assert_eq!(with_port("1.2.3.4"), format!("1.2.3.4:{DEFAULT_PORT}"));
        assert_eq!(with_port("::1"), format!("[::1]:{DEFAULT_PORT}"));
        assert_eq!(with_port("[::1]:80"), "[::1]:80");
    }

    #[tokio::test]
    async fn only_allowed_keys_get_in_and_the_host_key_is_pinned() {
        let host = tempfile::tempdir().unwrap();
        let mac = tempfile::tempdir().unwrap();
        let (endpoint, host_id) = host_endpoint("127.0.0.1:0".parse().unwrap(), host.path()).unwrap();
        let addr = endpoint.local_addr().unwrap().to_string();
        let home = host.path().to_owned();
        tokio::spawn(serve(endpoint, home));
        // a port on the host, to forward to.
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut b = [0u8; 5];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&b).await.unwrap();
        });

        let me = Identity::load_or_make(mac.path(), "client").unwrap();
        // not on the list: the handshake fails, or the first stream does.
        let refused = match Link::connect(&addr, Some(&host_id.fingerprint()), &me).await {
            Err(_) => true,
            Ok(link) => link.open(&format!("tcp {port}")).await.is_err(),
        };
        assert!(refused, "a key nobody allowed got in");

        identity::allow(host.path(), &me.fingerprint(), "test mac").unwrap();
        // the wrong pin: refused before anything is sent.
        let err = Link::connect(&addr, Some("SHA256:notthehostskeyatallnotthehostskeyatall"), &me).await.err().expect("a wrong pin connected");
        assert!(format!("{err:#}").contains("not the"), "{err:#}");

        let link = Link::connect(&addr, Some(&host_id.fingerprint()), &me).await.unwrap();
        assert_eq!(link.fingerprint, host_id.fingerprint());
        let mut s = link.open(&format!("tcp {port}")).await.unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        let err = link.open("telnet").await.err().unwrap();
        assert!(err.to_string().contains("doesn't do"), "{err}");
    }
}
