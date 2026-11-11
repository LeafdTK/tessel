//! talk to a photon daemon over its socket.
//!
//! one connection, any number of concurrent requests, and a stream of events.
//! used by the cli and the app; nothing in here knows about either.

use photon_proto::frame::{self, Decoder, KIND_JSON};
use photon_proto::{Envelope, ErrorCode, Event, PROTO_MAJOR, PROTO_MINOR, Request, Response, RpcError};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::{mpsc, oneshot};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// the daemon answered, and the answer was no. said once: the error is its own message.
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("the connection to the daemon closed")]
    Closed,
    #[error("could not reach the daemon at {path}: {source}")]
    Connect { path: PathBuf, source: std::io::Error },
    #[error("could not start the daemon: {0}")]
    Start(String),
}

impl ClientError {
    pub fn is_version_mismatch(&self) -> bool {
        matches!(self, Self::Rpc(e) if e.code == ErrorCode::VersionMismatch)
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Response, RpcError>>>>>;

pub struct Client {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: AtomicU64,
}

/// events from the daemon. ends when the connection does.
pub type Events = mpsc::UnboundedReceiver<Event>;

#[derive(Clone, Debug)]
pub struct Welcome {
    pub proto_major: u16,
    pub proto_minor: u16,
    pub build: String,
    pub daemon_pid: u32,
    pub capabilities: Vec<String>,
}

impl Client {
    /// connect to a daemon on this machine and shake hands.
    pub async fn connect(socket: &Path, name: &str) -> Result<(Self, Events, Welcome), ClientError> {
        let stream = UnixStream::connect(socket)
            .await
            .map_err(|source| ClientError::Connect { path: socket.to_owned(), source })?;
        Self::over(stream, name).await
    }

    /// shake hands over any byte stream. the protocol doesn't care what carries
    /// it: a unix socket today, a quic stream or a websocket for a daemon on
    /// another machine, each just another `connect` that ends here.
    pub async fn over<S>(stream: S, name: &str) -> Result<(Self, Events, Welcome), ClientError>
    where
        S: AsyncRead + AsyncWrite + Send + 'static,
    {
        let (mut read, mut write) = tokio::io::split(stream);
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Pending = Default::default();

        tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                if write.write_all(&bytes).await.is_err() {
                    break;
                }
            }
            // the client was dropped: close our side, so the daemon hears it went
            // (a split stream doesn't close until both halves are gone).
            let _ = write.shutdown().await;
        });

        let waiting = pending.clone();
        tokio::spawn(async move {
            let mut decoder = Decoder::new();
            let mut buf = vec![0u8; 64 * 1024];
            'read: loop {
                let n = match read.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                decoder.push(&buf[..n]);
                loop {
                    let frame = match decoder.next_frame() {
                        Ok(Some(f)) => f,
                        Ok(None) => break,
                        Err(_) => break 'read,
                    };
                    if frame.kind != KIND_JSON {
                        continue;
                    }
                    let envelope = serde_json::from_slice::<Envelope>(&frame.payload);
                    // an answer we can't read still answers its request, with why:
                    // a caller is never left waiting on something that already came.
                    if let Err(e) = &envelope
                        && let Some(id) = serde_json::from_slice::<serde_json::Value>(&frame.payload).ok().and_then(|v| v.get("id").and_then(serde_json::Value::as_u64))
                    {
                        if let Some(tx) = waiting.lock().unwrap().remove(&id) {
                            let _ = tx.send(Err(RpcError::new(ErrorCode::Internal, format!("couldn't read the daemon's answer: {e}"))));
                        }
                        continue;
                    }
                    match envelope {
                        Ok(Envelope::Res { id, ok, err }) => {
                            if let Some(tx) = waiting.lock().unwrap().remove(&id) {
                                let result = match (ok, err) {
                                    (_, Some(e)) => Err(e),
                                    (Some(r), None) => Ok(r),
                                    (None, None) => Ok(Response::Unknown),
                                };
                                let _ = tx.send(result);
                            }
                        }
                        Ok(Envelope::Ev { ev }) => {
                            if ev_tx.send(ev).is_err() {
                                // nobody is listening for events; keep serving requests.
                            }
                        }
                        // something from a newer daemon we don't understand: skip it.
                        _ => {}
                    }
                }
            }
            // wake everyone still waiting: their answer is never coming.
            waiting.lock().unwrap().clear();
        });

        let client = Self { out, pending, next: AtomicU64::new(1) };
        let hello = Request::Hello {
            proto_major: PROTO_MAJOR,
            proto_minor: PROTO_MINOR,
            build: env!("CARGO_PKG_VERSION").to_owned(),
            client: name.to_owned(),
            auth: None,
        };
        match client.request(hello).await? {
            Response::Welcome { proto_major, proto_minor, build, daemon_pid, capabilities } => {
                Ok((client, ev_rx, Welcome { proto_major, proto_minor, build, daemon_pid, capabilities }))
            }
            other => Err(RpcError::new(ErrorCode::Internal, format!("unexpected reply to hello: {other:?}")).into()),
        }
    }

    pub async fn request(&self, req: Request) -> Result<Response, ClientError> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let json = serde_json::to_vec(&Envelope::Req { id, req }).map_err(|e| RpcError::new(ErrorCode::Invalid, e.to_string()))?;
        let bytes = frame::encode(KIND_JSON, &json).map_err(|e| RpcError::new(ErrorCode::Invalid, e.to_string()))?;
        if self.out.send(bytes).is_err() {
            self.pending.lock().unwrap().remove(&id);
            return Err(ClientError::Closed);
        }
        match rx.await {
            Ok(result) => Ok(result?),
            Err(_) => Err(ClientError::Closed),
        }
    }
}

/// connect to the daemon for `home`, starting one in the background if none is running.
pub async fn connect_or_start(home: &Path, name: &str) -> Result<(Client, Events, Welcome), ClientError> {
    let socket = photon_proto::local::socket(home);
    match Client::connect(&socket, name).await {
        Ok(c) => return Ok(c),
        // a daemon is there and said no (wrong version): starting another won't help.
        Err(e @ ClientError::Rpc(_)) => return Err(e),
        Err(_) => {}
    }
    start_daemon(home)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        match Client::connect(&socket, name).await {
            Ok(c) => return Ok(c),
            Err(e @ ClientError::Rpc(_)) => return Err(e),
            Err(e) if tokio::time::Instant::now() >= deadline => {
                return Err(ClientError::Start(format!("it did not come up within 10s ({e}); see {}", photon_proto::local::log(home).display())));
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
        }
    }
}

/// where the daemon binary is: `$PHOTON_DAEMON_BIN`, next to this executable, or on PATH.
pub fn daemon_binary() -> PathBuf {
    if let Some(p) = std::env::var_os("PHOTON_DAEMON_BIN") {
        return p.into();
    }
    if let Ok(me) = std::env::current_exe()
        && let Some(dir) = me.parent()
    {
        let sibling = dir.join("photond");
        if sibling.is_file() {
            return sibling;
        }
    }
    PathBuf::from("photond")
}

/// start the daemon so that it outlives us: its own session, no terminal, output to its log.
pub fn start_daemon(home: &Path) -> Result<(), ClientError> {
    use std::os::unix::process::CommandExt;
    std::fs::create_dir_all(home).map_err(|e| ClientError::Start(format!("creating {}: {e}", home.display())))?;
    let log_path = photon_proto::local::log(home);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .map_err(|e| ClientError::Start(format!("opening {}: {e}", log_path.display())))?;
    let err = log.try_clone().map_err(|e| ClientError::Start(e.to_string()))?;
    let bin = daemon_binary();
    let mut cmd = std::process::Command::new(&bin);
    cmd.env("PHOTON_HOME", home).stdin(std::process::Stdio::null()).stdout(log).stderr(err);
    unsafe {
        cmd.pre_exec(|| {
            // a new session: closing the terminal or quitting the app that
            // started it must not take the daemon down.
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map(|_| ()).map_err(|e| ClientError::Start(format!("running {}: {e}", bin.display())))
}
