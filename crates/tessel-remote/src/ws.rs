//! the link to a host that only takes https: a vercel sandbox, whose ports
//! are reached through vercel's proxy (tls, http, websockets; no udp, so no
//! quic). every stream is a websocket of its own, carrying exactly what a
//! quic stream does: a first line saying what for (`tessel`, `tcp PORT`),
//! the host's `ok`, then bytes.
//!
//! the host lets in whoever shows its token (`Authorization: Bearer …`), a
//! secret made when it was set up and kept on both sides, readable by their
//! owner only.

use anyhow::{Context, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};

/// the port the host's websocket service listens on.
pub const PORT: u16 = 7733;

/// where a host keeps the token macs must show.
pub fn token_file(home: &Path) -> PathBuf {
    crate::identity::dir(home).join("ws-token")
}

/// a new token: 32 random bytes, url-safe base64.
pub fn new_token() -> anyhow::Result<String> {
    use base64::Engine;
    let mut b = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut b).map_err(|_| anyhow::anyhow!("no randomness"))?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b))
}

/// equal, in a time that doesn't say how much of it was.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// MARK: - the host

/// listen on `port` and relay for `home` every websocket that shows the token.
pub async fn serve(home: &Path, port: u16) -> anyhow::Result<()> {
    let token = std::fs::read_to_string(token_file(home)).with_context(|| format!("no token in {} (set the host up again)", token_file(home).display()))?.trim().to_owned();
    anyhow::ensure!(token.len() >= 32, "the token in {} is too short", token_file(home).display());
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.with_context(|| format!("listening on tcp {port}"))?;
    eprintln!("tessel remote: websockets on tcp {port}, for {}", home.display());
    let token = Arc::new(token);
    loop {
        let (tcp, from) = match listener.accept().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("tessel remote: accept: {e}");
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let (token, home) = (token.clone(), home.to_owned());
        tokio::spawn(async move {
            let _ = tcp.set_nodelay(true);
            let check = |req: &Request, res: Response| -> Result<Response, ErrorResponse> {
                let shown = req.headers().get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or("");
                if same(shown, &token) {
                    Ok(res)
                } else {
                    let mut no = ErrorResponse::new(Some("not allowed".into()));
                    *no.status_mut() = tokio_tungstenite::tungstenite::http::StatusCode::UNAUTHORIZED;
                    Err(no)
                }
            };
            match tokio_tungstenite::accept_hdr_async(tcp, check).await {
                Ok(ws) => {
                    if let Err(e) = crate::quic::relay_stream(bytes_of(ws), &home).await {
                        eprintln!("tessel remote: {from}: {e:#}");
                    }
                }
                Err(e) => eprintln!("tessel remote: {from} turned away: {e}"),
            }
        });
    }
}

/// a websocket as a byte stream: binary messages in, binary messages out.
fn bytes_of<S>(ws: tokio_tungstenite::WebSocketStream<S>) -> tokio::io::DuplexStream
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    use futures_util::{SinkExt, StreamExt};
    let (ours, theirs) = tokio::io::duplex(256 * 1024);
    let (mut sink, mut source) = ws.split();
    let (mut rd, mut wr) = tokio::io::split(theirs);
    // what arrives, to whoever reads.
    tokio::spawn(async move {
        while let Some(Ok(msg)) = source.next().await {
            let data = match msg {
                Message::Binary(b) => b.to_vec(),
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Close(_) => break,
                _ => continue,
            };
            if wr.write_all(&data).await.is_err() {
                break;
            }
        }
        let _ = wr.shutdown().await;
    });
    // what's written, out; and a ping now and then, so proxies keep it open.
    tokio::spawn(async move {
        let mut buf = vec![0u8; 64 * 1024];
        let mut tick = tokio::time::interval(Duration::from_secs(20));
        tick.tick().await;
        loop {
            tokio::select! {
                n = rd.read(&mut buf) => match n {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if sink.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                            return;
                        }
                    }
                },
                _ = tick.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        return;
                    }
                }
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });
    ours
}

// MARK: - the mac

/// a host reached by websockets: where, and the token to show.
#[derive(Clone)]
pub struct Link {
    url: String,
    token: String,
    /// how long the first handshake took, there and back.
    pub rtt: Duration,
    /// a sandbox kept running for as long as this is held.
    _lease: Option<Arc<crate::vercel::Lease>>,
}

fn tls() -> Arc<rustls::ClientConfig> {
    static TLS: std::sync::OnceLock<Arc<rustls::ClientConfig>> = std::sync::OnceLock::new();
    TLS.get_or_init(|| {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .expect("tls versions")
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    })
    .clone()
}

impl Link {
    /// `url` is the host's `https://…` (or `wss://…`); a first stream proves it answers.
    pub async fn connect(url: &str, token: &str) -> anyhow::Result<Link> {
        let url = url.trim_end_matches('/').replacen("https://", "wss://", 1).replacen("http://", "ws://", 1);
        let mut link = Link { url, token: token.to_owned(), rtt: Duration::ZERO, _lease: None };
        let t = Instant::now();
        let probe = link.socket().await?;
        link.rtt = t.elapsed();
        drop(probe);
        Ok(link)
    }

    async fn socket(&self) -> anyhow::Result<tokio::io::DuplexStream> {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = format!("{}/tessel", self.url).into_client_request()?;
        req.headers_mut().insert("authorization", format!("Bearer {}", self.token).parse()?);
        let uri = req.uri().clone();
        let host = uri.host().context("no host in the url")?.to_owned();
        let secure = uri.scheme_str() == Some("wss");
        let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
        let tcp = tokio::time::timeout(Duration::from_secs(10), tokio::net::TcpStream::connect((host.as_str(), port))).await.context("no answer")??;
        let _ = tcp.set_nodelay(true);
        let shake = async {
            if secure {
                let name = rustls::pki_types::ServerName::try_from(host.clone())?;
                let stream = tokio_rustls::TlsConnector::from(tls()).connect(name, tcp).await?;
                let (ws, _) = tokio_tungstenite::client_async(req, stream).await.map_err(explain)?;
                anyhow::Ok(bytes_of(ws))
            } else {
                let (ws, _) = tokio_tungstenite::client_async(req, tcp).await.map_err(explain)?;
                anyhow::Ok(bytes_of(ws))
            }
        };
        tokio::time::timeout(Duration::from_secs(15), shake).await.context("the host didn't answer in time")?
    }

    /// the same link, keeping `lease` for as long as it (or a clone) lives.
    pub fn held_by(mut self, lease: crate::vercel::Lease) -> Self {
        self._lease = Some(Arc::new(lease));
        self
    }

    /// a stream for `what` (`tessel`, or `tcp PORT`), once the host says yes.
    pub async fn open(&self, what: &str) -> anyhow::Result<BufReader<tokio::io::DuplexStream>> {
        let mut s = BufReader::new(self.socket().await?);
        s.write_all(format!("{what}\n").as_bytes()).await?;
        s.flush().await?;
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(15), (&mut s).take(1024).read_line(&mut line)).await.context("the host didn't answer")??;
        match line.trim() {
            "ok" => Ok(s),
            "" => bail!("the host closed the stream"),
            other => bail!("{}", other.strip_prefix("no ").unwrap_or(other)),
        }
    }
}

/// why a handshake failed, said plainly.
fn explain(e: tokio_tungstenite::tungstenite::Error) -> anyhow::Error {
    use tokio_tungstenite::tungstenite::Error;
    match e {
        Error::Http(r) if r.status() == 401 => anyhow::anyhow!("the host didn't take this mac's token (set it up again)"),
        Error::Http(r) if r.status().as_u16() >= 500 => anyhow::anyhow!("nothing answers there yet ({})", r.status()),
        Error::Http(r) => anyhow::anyhow!("the host said {}", r.status()),
        e => anyhow::anyhow!(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn only_the_token_gets_in_and_streams_carry_bytes() {
        let host = tempfile::tempdir().unwrap();
        let token = new_token().unwrap();
        std::fs::create_dir_all(crate::identity::dir(host.path())).unwrap();
        std::fs::write(token_file(host.path()), &token).unwrap();
        let port = {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            l.local_addr().unwrap().port()
        };
        let home = host.path().to_owned();
        tokio::spawn(async move { serve(&home, port).await.unwrap() });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let echo = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let eport = echo.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut s, _) = echo.accept().await.unwrap();
            let mut b = [0u8; 5];
            s.read_exact(&mut b).await.unwrap();
            s.write_all(&b).await.unwrap();
        });
        let url = format!("http://127.0.0.1:{port}");
        assert!(Link::connect(&url, "not-the-token").await.is_err(), "a wrong token is turned away");
        let link = Link::connect(&url, &token).await.unwrap();
        let mut s = link.open(&format!("tcp {eport}")).await.unwrap();
        s.write_all(b"hello").await.unwrap();
        let mut back = [0u8; 5];
        s.read_exact(&mut back).await.unwrap();
        assert_eq!(&back, b"hello");
        assert!(link.open("telnet").await.is_err());
    }
}
