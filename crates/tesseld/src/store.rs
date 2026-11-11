//! sqlite. one connection, on one thread, fed by a channel: writes never
//! contend and callers never block on disk.
//!
//! what's in here is a cache. the per-session raw logs are the truth and the
//! tables can be rebuilt from them, which is why changing the event model
//! never needs a data migration.
//!
//! schema note: a session row is a bag of independent nullable columns.
//! there is no `kind` column and there must never be one.

use crate::reducer::{Meta, Persisted};
use anyhow::Context;
use tessel_proto::{Attention, AttentionReason, Item, SessionId, WorktreeInfo};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::Path;
use std::sync::mpsc;
use tokio::sync::oneshot;

const SCHEMA_VERSION: i64 = 2;

enum Job {
    Save { session: Box<Persisted>, items: Vec<Item> },
    Delete { id: SessionId },
    LoadSessions { reply: oneshot::Sender<anyhow::Result<Vec<Persisted>>> },
    LoadItems { id: SessionId, reply: oneshot::Sender<anyhow::Result<Vec<Item>>> },
    /// resolves once everything queued before it is on disk.
    Flush { reply: oneshot::Sender<()> },
}

#[derive(Clone)]
pub struct Store {
    tx: mpsc::Sender<Job>,
}

impl Store {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        migrate(&conn)?;
        let (tx, rx) = mpsc::channel::<Job>();
        std::thread::Builder::new().name("tessel-store".into()).spawn(move || run(conn, rx))?;
        Ok(Self { tx })
    }

    /// queue a session row and the items that changed. returns immediately.
    pub fn save(&self, session: Persisted, items: Vec<Item>) {
        let _ = self.tx.send(Job::Save { session: Box::new(session), items });
    }

    pub fn delete(&self, id: SessionId) {
        let _ = self.tx.send(Job::Delete { id });
    }

    pub async fn load_sessions(&self) -> anyhow::Result<Vec<Persisted>> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Job::LoadSessions { reply }).ok().context("store thread is gone")?;
        rx.await.context("store thread is gone")?
    }

    pub async fn load_items(&self, id: SessionId) -> anyhow::Result<Vec<Item>> {
        let (reply, rx) = oneshot::channel();
        self.tx.send(Job::LoadItems { id, reply }).ok().context("store thread is gone")?;
        rx.await.context("store thread is gone")?
    }

    pub async fn flush(&self) {
        let (reply, rx) = oneshot::channel();
        if self.tx.send(Job::Flush { reply }).is_ok() {
            let _ = rx.await;
        }
    }
}

fn migrate(conn: &Connection) -> anyhow::Result<()> {
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    anyhow::ensure!(
        version <= SCHEMA_VERSION,
        "this database was written by a newer tessel (schema {version}, this build knows {SCHEMA_VERSION})"
    );
    if version < 1 {
        conn.execute_batch(
            "BEGIN;
             CREATE TABLE sessions (
                 id               TEXT PRIMARY KEY,
                 goal             TEXT NOT NULL,
                 cwd              TEXT NOT NULL,
                 worktree_repo    TEXT,
                 worktree_branch  TEXT,
                 parent           TEXT,
                 created_ms       INTEGER NOT NULL,
                 updated_ms       INTEGER NOT NULL,
                 seq              INTEGER NOT NULL,
                 attention_reason TEXT,
                 attention_since  INTEGER,
                 attention_seq    INTEGER,
                 model            TEXT,
                 agent_session_id TEXT,
                 cost_usd         REAL NOT NULL DEFAULT 0,
                 turn_open        INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE items (
                 session TEXT NOT NULL,
                 id      TEXT NOT NULL,
                 ord     INTEGER NOT NULL,
                 body    TEXT NOT NULL,
                 PRIMARY KEY (session, id)
             );
             CREATE INDEX items_by_order ON items (session, ord);
             PRAGMA user_version = 1;
             COMMIT;",
        )?;
    }
    if version < 2 {
        conn.execute_batch(
            "BEGIN;
             ALTER TABLE sessions ADD COLUMN permission_mode TEXT;
             ALTER TABLE sessions ADD COLUMN context_used INTEGER;
             ALTER TABLE sessions ADD COLUMN context_window INTEGER;
             ALTER TABLE sessions ADD COLUMN linked_dirs TEXT;
             PRAGMA user_version = 2;
             COMMIT;",
        )?;
    }
    Ok(())
}

fn run(mut conn: Connection, rx: mpsc::Receiver<Job>) {
    while let Ok(job) = rx.recv() {
        match job {
            Job::Save { session, items } => {
                if let Err(e) = save(&mut conn, &session, &items) {
                    eprintln!("tesseld: store: saving session {}: {e:#}", session.meta.id);
                }
            }
            Job::Delete { id } => {
                let id = id.to_string();
                let r = conn
                    .execute("DELETE FROM items WHERE session = ?1", params![id])
                    .and_then(|_| conn.execute("DELETE FROM sessions WHERE id = ?1", params![id]));
                if let Err(e) = r {
                    eprintln!("tesseld: store: deleting session {id}: {e:#}");
                }
            }
            Job::LoadSessions { reply } => {
                let _ = reply.send(load_sessions(&conn));
            }
            Job::LoadItems { id, reply } => {
                let _ = reply.send(load_items(&conn, id));
            }
            Job::Flush { reply } => {
                let _ = reply.send(());
            }
        }
    }
}

fn save(conn: &mut Connection, p: &Persisted, items: &[Item]) -> anyhow::Result<()> {
    let tx = conn.transaction()?;
    let reason = p.attention.as_ref().map(|a| serde_json::to_value(a.reason)).transpose()?;
    let reason = reason.as_ref().and_then(|v| v.as_str());
    tx.execute(
        "INSERT INTO sessions (id, goal, cwd, worktree_repo, worktree_branch, parent, created_ms, updated_ms, seq,
                               attention_reason, attention_since, attention_seq, model, agent_session_id, cost_usd, turn_open,
                               permission_mode, context_used, context_window, linked_dirs)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)
         ON CONFLICT(id) DO UPDATE SET
             goal = excluded.goal, cwd = excluded.cwd, worktree_repo = excluded.worktree_repo,
             worktree_branch = excluded.worktree_branch, parent = excluded.parent, updated_ms = excluded.updated_ms,
             seq = excluded.seq, attention_reason = excluded.attention_reason, attention_since = excluded.attention_since,
             attention_seq = excluded.attention_seq, model = excluded.model, agent_session_id = excluded.agent_session_id,
             cost_usd = excluded.cost_usd, turn_open = excluded.turn_open, permission_mode = excluded.permission_mode,
             context_used = excluded.context_used, context_window = excluded.context_window, linked_dirs = excluded.linked_dirs",
        params![
            p.meta.id.to_string(),
            p.meta.goal,
            p.meta.cwd,
            p.meta.worktree.as_ref().map(|w| w.repo.as_str()),
            p.meta.worktree.as_ref().map(|w| w.branch.as_str()),
            p.meta.parent.map(|s| s.to_string()),
            p.meta.created_ms as i64,
            p.updated_ms as i64,
            p.seq as i64,
            reason,
            p.attention.as_ref().map(|a| a.since_ms as i64),
            p.attention.as_ref().map(|a| a.seq as i64),
            p.model,
            p.agent_session_id,
            p.cost_usd,
            p.turn_open,
            p.permission_mode,
            p.context_used.map(|n| n as i64),
            p.context_window.map(|n| n as i64),
            (!p.meta.linked_dirs.is_empty()).then(|| serde_json::to_string(&p.meta.linked_dirs)).transpose()?,
        ],
    )?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO items (session, id, ord, body) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(session, id) DO UPDATE SET body = excluded.body",
        )?;
        let sid = p.meta.id.to_string();
        for item in items {
            stmt.execute(params![sid, item.id.as_str(), item.order as i64, serde_json::to_string(item)?])?;
        }
    }
    tx.commit()?;
    Ok(())
}

fn load_sessions(conn: &Connection) -> anyhow::Result<Vec<Persisted>> {
    let mut stmt = conn.prepare(
        "SELECT id, goal, cwd, worktree_repo, worktree_branch, parent, created_ms, updated_ms, seq,
                attention_reason, attention_since, attention_seq, model, agent_session_id, cost_usd, turn_open,
                permission_mode, context_used, context_window, linked_dirs
         FROM sessions ORDER BY created_ms",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, i64>(6)?,
            r.get::<_, i64>(7)?,
            r.get::<_, i64>(8)?,
            r.get::<_, Option<String>>(9)?,
            r.get::<_, Option<i64>>(10)?,
            r.get::<_, Option<i64>>(11)?,
            r.get::<_, Option<String>>(12)?,
            r.get::<_, Option<String>>(13)?,
            r.get::<_, f64>(14)?,
            r.get::<_, bool>(15)?,
            r.get::<_, Option<String>>(16)?,
            r.get::<_, Option<i64>>(17)?,
            r.get::<_, Option<i64>>(18)?,
            r.get::<_, Option<String>>(19)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, goal, cwd, wt_repo, wt_branch, parent, created, updated, seq, reason, since, aseq, model, agent_sid, cost, turn_open, mode, used, window, linked) = row?;
        // a row we can't make sense of is skipped, not fatal: one bad session must not take the rest down.
        let Ok(id) = id.parse::<SessionId>() else {
            eprintln!("tesseld: store: skipping session with unreadable id {id:?}");
            continue;
        };
        let attention = match (reason, since, aseq) {
            (Some(reason), Some(since), Some(aseq)) => {
                let reason: AttentionReason = serde_json::from_value(serde_json::Value::String(reason)).unwrap_or(AttentionReason::Unknown);
                Some(Attention { reason, since_ms: since as u64, seq: aseq as u64 })
            }
            _ => None,
        };
        let worktree = match (wt_repo, wt_branch) {
            (Some(repo), Some(branch)) => Some(WorktreeInfo { repo, branch }),
            _ => None,
        };
        out.push(Persisted {
            meta: Meta {
                id,
                goal,
                cwd,
                worktree,
                parent: parent.and_then(|p| p.parse().ok()),
                created_ms: created as u64,
                linked_dirs: linked.and_then(|l| serde_json::from_str(&l).ok()).unwrap_or_default(),
            },
            seq: seq as u64,
            attention,
            model,
            permission_mode: mode,
            context_used: used.map(|n| n as u64),
            context_window: window.map(|n| n as u64),
            agent_session_id: agent_sid,
            cost_usd: cost,
            turn_open,
            updated_ms: updated as u64,
        });
    }
    Ok(out)
}

fn load_items(conn: &Connection, id: SessionId) -> anyhow::Result<Vec<Item>> {
    let mut stmt = conn.prepare("SELECT body FROM items WHERE session = ?1 ORDER BY ord")?;
    let rows = stmt.query_map(params![id.to_string()], |r| r.get::<_, String>(0))?;
    let mut out = Vec::new();
    for body in rows {
        match serde_json::from_str::<Item>(&body?) {
            Ok(item) => out.push(item),
            Err(e) => eprintln!("tesseld: store: skipping unreadable item in {id}: {e}"),
        }
    }
    Ok(out)
}

/// for tests and tools: is there a row for this session?
pub fn session_exists(path: &Path, id: SessionId) -> anyhow::Result<bool> {
    let conn = Connection::open(path)?;
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM sessions WHERE id = ?1", params![id.to_string()], |r| r.get(0))
        .optional()?;
    Ok(found.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tessel_proto::{ItemBody, ItemId};

    fn persisted(id: SessionId) -> Persisted {
        Persisted {
            meta: Meta {
                id,
                goal: "ship it".into(),
                cwd: "/work/repo".into(),
                worktree: Some(WorktreeInfo { repo: "/work/repo".into(), branch: "tessel/x".into() }),
                parent: Some(SessionId::new()),
                created_ms: 10,
                linked_dirs: vec!["/work/other".into()],
            },
            seq: 7,
            attention: Some(Attention { reason: AttentionReason::Finished, since_ms: 99, seq: 6 }),
            model: Some("m".into()),
            permission_mode: Some("plan".into()),
            context_used: Some(41_000),
            context_window: Some(1_000_000),
            agent_session_id: Some("agent-1".into()),
            cost_usd: 1.25,
            turn_open: true,
            updated_ms: 100,
        }
    }

    fn item(id: &str, order: u64, text: &str) -> Item {
        Item { id: ItemId::from(id), parent: None, seq: order, order, created_ms: 1, body: ItemBody::Text { text: text.into(), streaming: false } }
    }

    #[tokio::test]
    async fn a_session_and_its_items_survive_a_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        let id = SessionId::new();
        let p = persisted(id);
        {
            let store = Store::open(&path).unwrap();
            store.save(p.clone(), vec![item("b", 2, "second"), item("a", 1, "first")]);
            // an upsert replaces the item, it does not duplicate it.
            store.save(p.clone(), vec![item("a", 1, "first, edited")]);
            store.flush().await;
        }
        let store = Store::open(&path).unwrap();
        assert_eq!(store.load_sessions().await.unwrap(), vec![p]);
        let items = store.load_items(id).await.unwrap();
        assert_eq!(items.iter().map(|i| i.id.as_str()).collect::<Vec<_>>(), ["a", "b"], "in transcript order");
        assert!(matches!(&items[0].body, ItemBody::Text { text, .. } if text == "first, edited"));
    }

    #[tokio::test]
    async fn every_optional_attribute_is_independently_absent() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("p.db")).unwrap();
        let mut p = persisted(SessionId::new());
        p.meta.worktree = None;
        p.meta.parent = None;
        p.attention = None;
        p.model = None;
        p.agent_session_id = None;
        p.turn_open = false;
        store.save(p.clone(), vec![]);
        assert_eq!(store.load_sessions().await.unwrap(), vec![p]);
    }

    #[tokio::test]
    async fn delete_removes_the_session_and_its_items() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        let store = Store::open(&path).unwrap();
        let (keep, gone) = (SessionId::new(), SessionId::new());
        store.save(persisted(keep), vec![item("a", 1, "x")]);
        store.save(persisted(gone), vec![item("a", 1, "y")]);
        store.delete(gone);
        store.flush().await;
        assert!(session_exists(&path, keep).unwrap());
        assert!(!session_exists(&path, gone).unwrap());
        assert!(store.load_items(gone).await.unwrap().is_empty());
        assert_eq!(store.load_items(keep).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn an_item_from_a_newer_build_still_loads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        let store = Store::open(&path).unwrap();
        let id = SessionId::new();
        store.save(persisted(id), vec![item("a", 1, "x")]);
        store.flush().await;
        let conn = Connection::open(&path).unwrap();
        conn.execute(
            "INSERT INTO items (session, id, ord, body) VALUES (?1, 'future', 2, ?2)",
            params![id.to_string(), r#"{"id":"future","seq":2,"order":2,"created_ms":1,"kind":"hologram","beam":true}"#],
        )
        .unwrap();
        conn.execute("INSERT INTO items (session, id, ord, body) VALUES (?1, 'junk', 3, 'not json')", params![id.to_string()]).unwrap();
        let items = store.load_items(id).await.unwrap();
        assert_eq!(items.len(), 2, "the unknown kind loads, the corrupt row is skipped");
        assert_eq!(items[1].body, ItemBody::Unknown);
    }

    #[test]
    fn a_database_from_an_older_tessel_is_brought_up_to_date_with_its_sessions_intact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        // exactly what version 1 wrote.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, goal TEXT NOT NULL, cwd TEXT NOT NULL, worktree_repo TEXT, worktree_branch TEXT,
                 parent TEXT, created_ms INTEGER NOT NULL, updated_ms INTEGER NOT NULL, seq INTEGER NOT NULL, attention_reason TEXT,
                 attention_since INTEGER, attention_seq INTEGER, model TEXT, agent_session_id TEXT, cost_usd REAL NOT NULL DEFAULT 0,
                 turn_open INTEGER NOT NULL DEFAULT 0);
             CREATE TABLE items (session TEXT NOT NULL, id TEXT NOT NULL, ord INTEGER NOT NULL, body TEXT NOT NULL, PRIMARY KEY (session, id));
             CREATE INDEX items_by_order ON items (session, ord);
             INSERT INTO sessions (id, goal, cwd, created_ms, updated_ms, seq, model, cost_usd)
                 VALUES ('00000000-0000-4000-8000-000000000001', 'old one', '/w', 1, 2, 3, 'm', 0.5);
             PRAGMA user_version = 1;",
        )
        .unwrap();
        drop(conn);
        drop(Store::open(&path).unwrap());
        let sessions = load_sessions(&Connection::open(&path).unwrap()).unwrap();
        assert_eq!(sessions.len(), 1);
        let p = &sessions[0];
        assert_eq!((p.meta.goal.as_str(), p.seq, p.model.as_deref(), p.cost_usd), ("old one", 3, Some("m"), 0.5));
        assert_eq!((p.permission_mode.clone(), p.context_used, p.context_window), (None, None, None));
    }

    #[test]
    fn a_database_from_the_future_is_refused_not_mangled() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.db");
        drop(Store::open(&path).unwrap());
        Connection::open(&path).unwrap().pragma_update(None, "user_version", 99).unwrap();
        let err = Store::open(&path).err().unwrap().to_string();
        assert!(err.contains("newer tessel"), "{err}");
    }
}
