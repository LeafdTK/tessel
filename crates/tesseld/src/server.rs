//! the socket. turns bytes into hub messages and envelopes into bytes.
//!
//! a connection is two tasks: a reader that decodes frames and forwards
//! requests, and a writer that drains this client's queue. the writer merges
//! what it can before writing, so a client that reads slowly gets fewer,
//! larger updates instead of a growing backlog.

use crate::hub::{CLIENT_QUEUE, ClientId, HubMsg};
use tessel_proto::frame::{self, Decoder, KIND_BINARY, KIND_JSON};
use tessel_proto::{Envelope, Event};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

static NEXT_CLIENT: AtomicU64 = AtomicU64::new(1);

/// what goes out to a client: an envelope, or a terminal's bytes.
#[derive(Debug)]
pub enum Out {
    Env(Envelope),
    Term(tessel_proto::term::TermId, Vec<u8>),
}

impl From<Envelope> for Out {
    fn from(e: Envelope) -> Self {
        Out::Env(e)
    }
}

/// a terminal's bytes are merged up to this much per frame.
const TERM_FRAME: usize = 1024 * 1024;

pub async fn serve(listener: UnixListener, hub: mpsc::UnboundedSender<HubMsg>) {
    loop {
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(e) => {
                eprintln!("tesseld: accept: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                continue;
            }
        };
        if !serve_stream(stream, &hub) {
            return;
        }
    }
}

/// serve one client over any byte stream. every listener (the unix socket
/// today; quic or websockets for remote clients later) hands its connections
/// here, so a remote client is the same client. false once the hub is gone.
pub fn serve_stream<S>(stream: S, hub: &mpsc::UnboundedSender<HubMsg>) -> bool
where
    S: AsyncRead + AsyncWrite + Send + 'static,
{
    let client: ClientId = NEXT_CLIENT.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel::<Out>(CLIENT_QUEUE);
    if hub.send(HubMsg::Connected { client, tx }).is_err() {
        return false;
    }
    let (read, write) = tokio::io::split(stream);
    let hub = hub.clone();
    tokio::spawn(async move {
        let writer = tokio::spawn(write_loop(write, rx));
        read_loop(read, client, &hub).await;
        let _ = hub.send(HubMsg::Disconnected { client });
        // the hub drops this client's sender on disconnect, which ends the writer.
        let _ = writer.await;
    });
    true
}

async fn read_loop(mut read: impl AsyncRead + Unpin, client: ClientId, hub: &mpsc::UnboundedSender<HubMsg>) {
    let mut decoder = Decoder::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = match read.read(&mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.next_frame() {
                Ok(None) => break,
                // a peer sending nonsense lengths is not worth talking to.
                Err(_) => return,
                Ok(Some(f)) => {
                    if f.kind == KIND_BINARY {
                        if hub.send(HubMsg::TermInput { payload: f.payload }).is_err() {
                            return;
                        }
                        continue;
                    }
                    if f.kind != KIND_JSON {
                        continue;
                    }
                    // an envelope we can't read (a newer client?) is skipped, not fatal.
                    if let Ok(Envelope::Req { id, req }) = serde_json::from_slice::<Envelope>(&f.payload)
                        && hub.send(HubMsg::Request { client, id, req }).is_err()
                    {
                        return;
                    }
                }
            }
        }
    }
}

async fn write_loop(mut write: impl AsyncWrite + Unpin, mut rx: mpsc::Receiver<Out>) {
    let mut batch = Vec::with_capacity(256);
    let mut bytes = Vec::with_capacity(64 * 1024);
    loop {
        batch.clear();
        if rx.recv_many(&mut batch, 1024).await == 0 {
            break;
        }
        bytes.clear();
        for out in merge(std::mem::take(&mut batch)) {
            let framed = match out {
                Out::Env(env) => {
                    let Ok(json) = serde_json::to_vec(&env) else { continue };
                    frame::encode(KIND_JSON, &json)
                }
                Out::Term(term, b) => frame::encode(KIND_BINARY, &tessel_proto::term::encode(term, &b)),
            };
            match framed {
                Ok(f) => bytes.extend_from_slice(&f),
                Err(e) => eprintln!("tesseld: dropping an oversized message: {e}"),
            }
        }
        if write.write_all(&bytes).await.is_err() {
            break;
        }
    }
    let _ = write.shutdown().await;
}

/// shrink a queue: envelopes as `coalesce` does, and a terminal's bytes that
/// follow each other into one frame. order is kept.
fn merge(batch: Vec<Out>) -> Vec<Out> {
    let mut out: Vec<Out> = Vec::with_capacity(batch.len());
    let mut envs: Vec<Envelope> = Vec::new();
    for o in batch {
        match o {
            Out::Env(e) => envs.push(e),
            Out::Term(term, b) => {
                if !envs.is_empty() {
                    out.extend(coalesce(std::mem::take(&mut envs)).into_iter().map(Out::Env));
                }
                match out.last_mut() {
                    Some(Out::Term(t, prev)) if *t == term && prev.len() + b.len() <= TERM_FRAME => prev.extend_from_slice(&b),
                    _ => out.push(Out::Term(term, b)),
                }
            }
        }
    }
    out.extend(coalesce(envs).into_iter().map(Out::Env));
    out
}

/// shrink a queue of envelopes without changing what it means:
/// - consecutive text deltas for the same item become one delta.
/// - a session summary is dropped if a newer one for the same session follows.
///
/// order is otherwise preserved, and nothing but those two is ever merged.
pub fn coalesce(batch: Vec<Envelope>) -> Vec<Envelope> {
    // index of the last summary per session, so older ones can be skipped.
    let mut last_summary = std::collections::HashMap::new();
    for (i, env) in batch.iter().enumerate() {
        if let Envelope::Ev { ev: Event::SessionUpdated { summary } } = env {
            last_summary.insert(summary.id, i);
        }
    }
    let mut out: Vec<Envelope> = Vec::with_capacity(batch.len());
    for (i, env) in batch.into_iter().enumerate() {
        match env {
            Envelope::Ev { ev: Event::SessionUpdated { summary } } => {
                if last_summary.get(&summary.id) == Some(&i) {
                    out.push(Envelope::Ev { ev: Event::SessionUpdated { summary } });
                }
            }
            Envelope::Ev { ev: Event::ItemDelta { session, item_id, offset, text } } => {
                if let Some(Envelope::Ev { ev: Event::ItemDelta { session: s, item_id: id, offset: o, text: t } }) = out.last_mut()
                    && *s == session
                    && *id == item_id
                    && *o + t.len() as u64 == offset
                {
                    t.push_str(&text);
                } else {
                    out.push(Envelope::Ev { ev: Event::ItemDelta { session, item_id, offset, text } });
                }
            }
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::{Item, ItemBody, ItemId, Response, RunState, SessionId, SessionSummary};

    fn delta(s: SessionId, item: &str, offset: u64, text: &str) -> Envelope {
        Envelope::Ev { ev: Event::ItemDelta { session: s, item_id: ItemId::from(item), offset, text: text.into() } }
    }

    fn summary(s: SessionId, seq: u64) -> Envelope {
        Envelope::Ev {
            ev: Event::SessionUpdated {
                summary: SessionSummary {
                    id: s, goal: "g".into(), cwd: "/".into(), worktree: None, parent: None, run_state: RunState::Working,
                    activity: None, activity_since_ms: None, attention: None, warm: true, background_tasks: 0, model: None,
                    permission_mode: None, effort: None, fast: false, context_used: None, context_window: None, tasks: vec![], mcp: vec![], linked_dirs: vec![],
                    cost_usd: 0.0, trace: vec![], asking: None, last_seq: seq, created_ms: 0, updated_ms: 0,
                },
            },
        }
    }

    fn upsert(s: SessionId, id: &str) -> Envelope {
        let item = Item { id: ItemId::from(id), parent: None, seq: 1, order: 1, created_ms: 0, body: ItemBody::Text { text: String::new(), streaming: true } };
        Envelope::Ev { ev: Event::ItemUpserted { session: s, item } }
    }

    /// what a client ends up with after applying a stream of envelopes.
    fn apply(envs: &[Envelope]) -> (std::collections::HashMap<String, String>, std::collections::HashMap<SessionId, u64>) {
        let mut text = std::collections::HashMap::<String, String>::new();
        let mut seqs = std::collections::HashMap::new();
        for e in envs {
            match e {
                Envelope::Ev { ev: Event::ItemDelta { item_id, offset, text: t, .. } } => {
                    let buf = text.entry(item_id.0.clone()).or_default();
                    assert_eq!(*offset as usize, buf.len(), "gap or overlap in {item_id}");
                    buf.push_str(t);
                }
                Envelope::Ev { ev: Event::SessionUpdated { summary } } => {
                    let prev = seqs.insert(summary.id, summary.last_seq).unwrap_or(0);
                    assert!(summary.last_seq >= prev, "summaries went backwards");
                }
                _ => {}
            }
        }
        (text, seqs)
    }

    #[test]
    fn a_backlog_of_deltas_becomes_one_with_no_gaps_or_duplicates() {
        let s = SessionId::new();
        let words: Vec<String> = (0..500).map(|i| format!("word{i} ")).collect();
        let mut offset = 0;
        let mut batch = Vec::new();
        for w in &words {
            batch.push(delta(s, "m/0", offset, w));
            offset += w.len() as u64;
        }
        let out = coalesce(batch.clone());
        assert_eq!(out.len(), 1, "500 queued deltas for one item collapse to a single write");
        assert_eq!(apply(&out).0["m/0"], words.concat());
        assert_eq!(apply(&out), apply(&batch), "the client ends up with exactly the same thing");
    }

    #[test]
    fn deltas_are_not_merged_across_items_sessions_or_other_events() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let batch = vec![
            delta(a, "x", 0, "one "),
            delta(a, "x", 4, "two "),
            upsert(a, "y"), // something else happened in between
            delta(a, "x", 8, "three "),
            delta(a, "y", 0, "other item"),
            delta(b, "x", 0, "other session"),
            delta(b, "x", 13, "!"),
        ];
        let out = coalesce(batch.clone());
        assert_eq!(out.len(), 5);
        assert!(matches!(&out[0], Envelope::Ev { ev: Event::ItemDelta { text, offset: 0, .. } } if text == "one two "));
        assert!(matches!(&out[1], Envelope::Ev { ev: Event::ItemUpserted { .. } }));
        assert!(matches!(&out[2], Envelope::Ev { ev: Event::ItemDelta { text, offset: 8, .. } } if text == "three "));
        assert!(matches!(&out[4], Envelope::Ev { ev: Event::ItemDelta { text, session, .. } } if text == "other session!" && *session == b));
    }

    #[test]
    fn a_delta_that_does_not_follow_on_is_left_alone() {
        let s = SessionId::new();
        // offset 9 does not continue "abc" (which ends at 3): never paper over a gap.
        let out = coalesce(vec![delta(s, "x", 0, "abc"), delta(s, "x", 9, "zzz")]);
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn only_the_newest_summary_per_session_survives_in_place() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let res = Envelope::Res { id: 7, ok: Some(Response::Ack), err: None };
        let batch = vec![summary(a, 1), summary(b, 1), summary(a, 2), res.clone(), summary(a, 3), upsert(a, "i")];
        let out = coalesce(batch.clone());
        assert_eq!(out, vec![summary(b, 1), res, summary(a, 3), upsert(a, "i")]);
        assert_eq!(apply(&out).1, apply(&batch).1);
    }

    #[test]
    fn responses_and_everything_else_pass_through_untouched_and_in_order() {
        let s = SessionId::new();
        let batch = vec![
            Envelope::Res { id: 1, ok: Some(Response::Ack), err: None },
            upsert(s, "a"),
            Envelope::Ev { ev: Event::Lagged },
            Envelope::Ev { ev: Event::SessionRemoved { session: s } },
            Envelope::Res { id: 2, ok: Some(Response::Ack), err: None },
        ];
        assert_eq!(coalesce(batch.clone()), batch);
        assert!(coalesce(Vec::new()).is_empty());
    }
}
