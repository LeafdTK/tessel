use super::*;
use crate::slack::{self, Outcome, Tasked};
use tessel_proto::task::TaskState;

const MONTH_MS: u64 = 30 * 86_400_000;

impl Hub {
    pub(super) fn slack_channels(&mut self, client: ClientId, id: u64) {
        let (tx, home, stub) = (self.tx.clone(), self.cfg.paths.home.clone(), self.cfg.slack.clone());
        tokio::spawn(async move {
            let listed = match slack::connect(home, stub).await {
                Ok(api) => slack::channels(api.as_ref()).await,
                Err(e) => Err(e),
            };
            let result = listed.map(|channels| Response::SlackChannels { channels }).map_err(|e| RpcError::new(ErrorCode::Invalid, e));
            let _ = tx.send(HubMsg::Reply { client, id, result });
        });
    }

    pub(super) fn slack_read(&mut self, repo: &str, channel: String, since: Option<i64>, note: String, ready: bool, reply: Option<(ClientId, u64)>) -> Result<(), RpcError> {
        let path = crate::projects::expand(repo);
        if !path.is_dir() {
            return Err(RpcError::new(ErrorCode::Invalid, format!("{repo} isn't a folder here")));
        }
        if channel.trim().trim_start_matches('#').is_empty() {
            return Err(RpcError::new(ErrorCode::Invalid, "Say which channel to read"));
        }
        let key = crate::board::repo_root(&path).to_string_lossy().into_owned();
        if !self.slack_reads.insert(key.clone()) {
            return Err(RpcError::new(ErrorCode::Conflict, "Slack is being read for this repository already"));
        }
        let now = now_ms();
        let existing: Vec<String> = self
            .tasks
            .values()
            .filter(|t| t.repo == key && t.state != TaskState::Dropped && !t.cleared && (t.state != TaskState::Landed || t.updated_ms >= now.saturating_sub(MONTH_MS)))
            .map(|t| t.title.clone())
            .collect();
        let tasks: HashMap<String, Tasked> = self.tasks.values().filter(|t| t.repo == key).map(|t| (t.id.clone(), Tasked { created_secs: (t.created_ms / 1000) as i64, title: t.title.clone() })).collect();
        let ask = slack::Ask {
            repo: key.clone(),
            channel,
            since,
            note,
            ledger: slack::load(&slack::ledger_path(&self.cfg.paths.home, &key)),
            tasks,
            existing,
            model: self.settings.role("triage"),
            now: (now / 1000) as i64,
        };
        let (tx, home, stub, backend) = (self.tx.clone(), self.cfg.paths.home.clone(), self.cfg.slack.clone(), self.backend.clone());
        tokio::spawn(async move {
            let result = match slack::connect(home, stub).await {
                Ok(api) => slack::read(api, backend, ask).await.map(Box::new),
                Err(e) => Err(e),
            };
            let _ = tx.send(HubMsg::SlackRead { repo: key, result, ready, reply });
        });
        Ok(())
    }

    pub(super) fn slack_read_done(&mut self, repo: String, result: Result<Box<Outcome>, String>, ready: bool, reply: Option<(ClientId, u64)>) {
        self.slack_reads.remove(&repo);
        let answer = match result {
            Ok(outcome) => Ok(self.slack_file(&repo, *outcome, ready)),
            Err(e) => Err(RpcError::new(ErrorCode::Invalid, e)),
        };
        if let Some((client, id)) = reply {
            self.respond(client, id, answer);
        }
    }

    fn slack_file(&mut self, repo: &str, o: Outcome, ready: bool) -> Response {
        let path = slack::ledger_path(&self.cfg.paths.home, repo);
        let mut ledger = slack::load(&path);
        let entry = ledger.channels.entry(o.channel.id.clone()).or_default();
        let shown = format!("#{}", o.channel.name);
        let mut filed = Vec::new();
        for t in o.tasks {
            let from: Vec<String> = t.from.into_iter().filter(|ts| !entry.used.contains_key(ts)).collect();
            let links: Vec<String> = from.iter().filter_map(|ts| o.links.get(ts).cloned()).collect();
            let spec = if links.is_empty() { t.spec } else { format!("{}\n\nFrom Slack: {}", t.spec.trim_end(), links.join(" ")) };
            let Ok(Response::Task { task }) = self.task_create(repo.to_owned(), t.title, spec, Vec::new(), None, false, None, false) else { continue };
            if let Some(made) = self.tasks.get_mut(&task.id) {
                made.source = links.first().cloned();
                if ready {
                    made.state = TaskState::Ready;
                }
            }
            self.note(&task.id, "tessel", format!("filed from Slack {shown}"));
            self.task_saved(&task.id);
            for ts in from {
                if o.roots.contains(&ts) {
                    entry.roots.insert(ts.clone());
                }
                entry.used.insert(ts, task.id.clone());
            }
            filed.push(task.id);
        }
        if let Some(w) = o.watermark
            && entry.last_read_ts.as_deref().is_none_or(|old| slack::newer(&w, old))
        {
            entry.last_read_ts = Some(w);
        }
        if let Err(e) = slack::save(&path, &ledger) {
            eprintln!("tesseld: keeping what was read from Slack: {e}");
        }
        if !filed.is_empty() {
            self.dispatch();
        }
        Response::SlackRead { read: o.read, filed, skipped: o.skipped, channel: shown }
    }
}
