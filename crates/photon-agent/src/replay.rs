//! a stand-in for claude that plays back a recording.
//!
//! it speaks through the same `Transport` as a real process, so everything
//! above the pipe (protocol, normalizer, daemon) runs for real. used by tests
//! and for poking at the ui without spending tokens.
//!
//! playback follows the recording's shape: where the recording shows us
//! writing something (a message, an answer to a prompt, an interrupt), the
//! player waits for the daemon to write *something* before carrying on.

use crate::backend::{Backend, SpawnOpts, Spawned};
use crate::claude::{Transport, drive};
use crate::recording::{Dir, RecLine};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{Notify, mpsc, oneshot};

pub struct ReplayBackend {
    /// one recording per spawn, in order; the last one repeats.
    recordings: Vec<Vec<RecLine>>,
    spawns: AtomicUsize,
    /// 0 = as fast as possible. otherwise the longest pause between lines.
    max_gap: Duration,
    /// everything written to any spawned agent, for tests to inspect.
    pub written: Arc<Mutex<Vec<String>>>,
    pub spawned: Arc<Mutex<Vec<SpawnOpts>>>,
}

impl ReplayBackend {
    pub fn new(recordings: Vec<Vec<RecLine>>) -> Self {
        assert!(!recordings.is_empty());
        Self {
            recordings,
            spawns: AtomicUsize::new(0),
            max_gap: Duration::ZERO,
            written: Default::default(),
            spawned: Default::default(),
        }
    }

    /// keep the recording's pacing, but never wait longer than `max_gap` between lines.
    pub fn paced(mut self, max_gap: Duration) -> Self {
        self.max_gap = max_gap;
        self
    }
}

impl Backend for ReplayBackend {
    fn models(&self) -> std::pin::Pin<Box<dyn std::future::Future<Output = anyhow::Result<Vec<photon_proto::ModelInfo>>> + Send>> {
        let model = |value: &str, id: &str, name: &str, description: &str| photon_proto::ModelInfo { value: value.into(), id: id.into(), name: name.into(), description: description.into() };
        let models = vec![
            model("default", "claude-opus-5-5", "Default (recommended)", "Opus 5.5 · Best for everyday, complex tasks"),
            model("opus", "claude-opus-5-5", "Opus 5.5", "For complex work and everyday tasks"),
            model("sonnet", "claude-sonnet-5-5", "Sonnet 5.5", "Most efficient for simpler tasks"),
            model("haiku", "claude-haiku-5-5", "Haiku 5.5", "Fastest for quick answers"),
        ];
        Box::pin(async move { Ok(models) })
    }

    fn spawn(&self, opts: SpawnOpts) -> anyhow::Result<Spawned> {
        let n = self.spawns.fetch_add(1, Ordering::SeqCst);
        let recs = self.recordings[n.min(self.recordings.len() - 1)].clone();
        self.spawned.lock().unwrap().push(opts.clone());

        let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
        let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<String>();
        let (exit_tx, exit_rx) = oneshot::channel();
        let killed = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let written = self.written.clone();
        let max_gap = self.max_gap;

        let (k, w) = (killed.clone(), wake.clone());
        tokio::spawn(async move {
            // a bare capture has no record of our side: it still needs a first message.
            let scripted_input = recs.iter().any(|r| r.dir == Dir::In);
            let mut need_input = !scripted_input;
            let mut last_t = 0;
            let mut code = Some(0);
            for rec in &recs {
                if k.load(Ordering::SeqCst) {
                    code = None;
                    break;
                }
                let wait_for_input = match rec.dir {
                    Dir::In => true,
                    Dir::Out => std::mem::take(&mut need_input),
                    Dir::Meta => false,
                };
                if wait_for_input {
                    tokio::select! {
                        line = stdin_rx.recv() => match line {
                            Some(l) => written.lock().unwrap().push(l),
                            None => { code = None; break; }
                        },
                        _ = w.notified() => { code = None; break; }
                    }
                }
                match rec.dir {
                    Dir::Out => {
                        if !max_gap.is_zero() {
                            let gap = Duration::from_millis(rec.t.saturating_sub(last_t)).min(max_gap);
                            tokio::select! {
                                _ = tokio::time::sleep(gap) => {}
                                _ = w.notified() => { code = None; break; }
                            }
                        }
                        last_t = rec.t;
                        if let Some(text) = rec.text()
                            && stdout_tx.send(text.to_owned()).is_err()
                        {
                            break;
                        }
                    }
                    Dir::Meta => {
                        if let Some(c) = rec.line.get("exit") {
                            code = c.as_i64().map(|c| c as i32);
                        }
                    }
                    Dir::In => {}
                }
            }
            if !k.load(Ordering::SeqCst) && code.is_some() {
                // a real agent stays up between turns until its input closes or it is told to stop.
                loop {
                    tokio::select! {
                        line = stdin_rx.recv() => match line {
                            Some(l) => written.lock().unwrap().push(l),
                            None => break,
                        },
                        _ = w.notified() => { code = None; break; }
                    }
                }
            }
            drop(stdout_tx);
            let _ = exit_tx.send(code);
        });

        let kill: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            killed.store(true, Ordering::SeqCst);
            wake.notify_one();
        });
        Ok(drive(Transport { stdin: stdin_tx, stdout: stdout_rx, exit: exit_rx, pid: None, kill }, opts.raw_log))
    }
}
