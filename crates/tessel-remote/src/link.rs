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
}

impl Link {
    /// a stream for `what` (`tessel`, or `tcp PORT`).
    pub async fn open(&self, what: &str) -> anyhow::Result<Box<dyn Io>> {
        Ok(match self {
            Link::Quic(l) => Box::new(l.open(what).await?),
            Link::Ws(l) => Box::new(l.open(what).await?),
        })
    }

    /// there and back.
    pub fn rtt(&self) -> Duration {
        match self {
            Link::Quic(l) => l.rtt(),
            Link::Ws(l) => l.rtt,
        }
    }

    /// what carries it, to say so.
    pub fn kind(&self) -> &'static str {
        match self {
            Link::Quic(_) => "quic",
            Link::Ws(_) => "wss",
        }
    }
}
