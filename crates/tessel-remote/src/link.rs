//! a host's link, whatever carries it: quic (a machine with a udp port), or
//! websockets (a vercel sandbox, behind an https proxy). streams are opened
//! the same way on both, and look the same once open.

use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};

/// one stream to a host: reads and writes like a socket.
pub trait Io: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> Io for T {}

#[derive(Clone)]
pub enum Link {
    Quic(crate::quic::Link),
    Ws(crate::ws::Link),
    /// quic by key, through any nat (with a lease, for a sandbox).
    P2p(crate::p2p::Link, Option<std::sync::Arc<crate::vercel::Lease>>),
}

impl Link {
    /// a stream for `what` (`tessel`, or `tcp PORT`).
    pub async fn open(&self, what: &str) -> anyhow::Result<Box<dyn Io>> {
        Ok(match self {
            Link::Quic(l) => Box::new(l.open(what).await?),
            Link::Ws(l) => Box::new(l.open(what).await?),
            Link::P2p(l, _) => Box::new(l.open(what).await?),
        })
    }

    /// there and back.
    pub fn rtt(&self) -> Duration {
        match self {
            Link::Quic(l) => l.rtt(),
            Link::Ws(l) => l.rtt,
            Link::P2p(l, _) => l.path().0,
        }
    }

    /// what carries it, to say so.
    pub fn kind(&self) -> &'static str {
        match self {
            Link::Quic(_) => "quic",
            Link::Ws(_) => "wss",
            Link::P2p(l, _) if l.path().1 => "direct quic",
            Link::P2p(..) => "quic via relay",
        }
    }
}

/// a stream used before the host has said yes: what's written goes at once
/// (a round trip sooner), and the host's `ok` (or `no WHY`) is taken off the
/// front of what's read, its no an error on the first read.
pub struct Answered<S> {
    inner: S,
    head: Option<Vec<u8>>,
    rest: Vec<u8>,
}

impl<S> Answered<S> {
    pub fn new(inner: S) -> Self {
        Self { inner, head: Some(Vec::new()), rest: Vec::new() }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Answered<S> {
    fn poll_read(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> std::task::Poll<std::io::Result<()>> {
        use std::task::Poll;
        let this = &mut *self;
        while let Some(head) = this.head.as_mut() {
            let mut tmp = [0u8; 1024];
            let mut rb = tokio::io::ReadBuf::new(&mut tmp);
            match std::pin::Pin::new(&mut this.inner).poll_read(cx, &mut rb) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(())) if rb.filled().is_empty() => return Poll::Ready(Err(std::io::Error::other("the host closed the stream"))),
                Poll::Ready(Ok(())) => head.extend_from_slice(rb.filled()),
            }
            if let Some(at) = head.iter().position(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(&head[..at]).trim().to_owned();
                if line != "ok" {
                    return Poll::Ready(Err(std::io::Error::other(line.strip_prefix("no ").unwrap_or(&line).to_owned())));
                }
                this.rest = head[at + 1..].to_vec();
                this.head = None;
            } else if head.len() > 1024 {
                return Poll::Ready(Err(std::io::Error::other("the host answered oddly")));
            }
        }
        if !this.rest.is_empty() {
            let n = this.rest.len().min(buf.remaining());
            buf.put_slice(&this.rest[..n]);
            this.rest.drain(..n);
            return Poll::Ready(Ok(()));
        }
        std::pin::Pin::new(&mut this.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Answered<S> {
    fn poll_write(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>, buf: &[u8]) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::Answered;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn the_hosts_answer_is_taken_off_the_front() {
        let (a, mut b) = tokio::io::duplex(1024);
        let mut s = Answered::new(a);
        s.write_all(b"sent before any answer").await.unwrap();
        b.write_all(b"ok\nhello").await.unwrap();
        let mut got = [0u8; 5];
        s.read_exact(&mut got).await.unwrap();
        assert_eq!(&got, b"hello");
        let (a, mut b) = tokio::io::duplex(1024);
        let mut s = Answered::new(a);
        b.write_all(b"no nothing on port 9\n").await.unwrap();
        let err = s.read(&mut [0u8; 4]).await.unwrap_err();
        assert_eq!(err.to_string(), "nothing on port 9");
    }
}
