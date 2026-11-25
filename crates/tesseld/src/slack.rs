use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper_util::rt::TokioIo;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use tessel_agent::backend::{Backend, Job};
use tessel_proto::task::SlackChannel;

pub const KEY: &str = "slack";
pub const NOT_CONNECTED: &str = "Slack isn't connected: add a key named slack in Settings › Keys";
pub const MOST_TASKS: usize = 15;
const DAY: i64 = 86_400;
const MOST_CHARS: usize = 150_000;
const MOST_PAGES: usize = 50;
const FOLLOW_UP_DAYS: i64 = 14;
const MOST_FOLLOW_UP_THREADS: usize = 20;

#[derive(Clone, Debug, PartialEq)]
pub enum Fail {
    Said(String),
    Other(String),
}

impl Fail {
    pub fn sentence(&self) -> String {
        match self {
            Fail::Said(code) => format!("Slack said: {code}"),
            Fail::Other(why) => why.clone(),
        }
    }
}

pub type Params<'a> = &'a [(&'a str, String)];
pub type Answer<'a> = Pin<Box<dyn Future<Output = Result<Value, Fail>> + Send + 'a>>;

pub trait Api: Send + Sync {
    fn call<'a>(&'a self, method: &'a str, params: Params<'a>) -> Answer<'a>;
}

impl<F> Api for F
where
    F: Fn(&str, &[(&str, String)]) -> Result<Value, Fail> + Send + Sync,
{
    fn call<'a>(&'a self, method: &'a str, params: Params<'a>) -> Answer<'a> {
        let got = self(method, params);
        Box::pin(async move { got })
    }
}

pub fn check(v: Value) -> Result<Value, Fail> {
    if v.get("ok").and_then(Value::as_bool) == Some(true) {
        return Ok(v);
    }
    let code = v.get("error").and_then(Value::as_str).unwrap_or("an error with no name").to_owned();
    Err(Fail::Said(match v.get("needed").and_then(Value::as_str) {
        Some(needed) => format!("{code} (it needs the {needed} scope)"),
        None => code,
    }))
}

type Sender = hyper::client::conn::http1::SendRequest<Empty<Bytes>>;

pub struct Web {
    token: String,
    tls: Arc<rustls::ClientConfig>,
    sender: tokio::sync::Mutex<Option<Sender>>,
    team: tokio::sync::OnceCell<Option<String>>,
}

const NEEDS_TEAM: [&str; 3] = ["conversations.list", "users.conversations", "users.list"];

const HOST: &str = "slack.com";

impl Web {
    pub fn new(token: String) -> Result<Self, String> {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .map_err(|e| e.to_string())?
            .with_root_certificates(roots)
            .with_no_client_auth();
        tls.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Self { token, tls: Arc::new(tls), sender: tokio::sync::Mutex::new(None), team: tokio::sync::OnceCell::new() })
    }

    async fn connect(&self) -> std::io::Result<Sender> {
        let tcp = tokio::net::TcpStream::connect((HOST, 443)).await?;
        let name = rustls::pki_types::ServerName::try_from(HOST).map_err(std::io::Error::other)?;
        let tls = tokio_rustls::TlsConnector::from(self.tls.clone()).connect(name, tcp).await?;
        let (s, conn) = hyper::client::conn::http1::handshake(TokioIo::new(tls)).await.map_err(std::io::Error::other)?;
        tokio::spawn(conn);
        Ok(s)
    }

    async fn get(&self, path: &str) -> Result<(u16, Option<u64>, Bytes), String> {
        let mut last = String::new();
        for _ in 0..2 {
            let mut guard = self.sender.lock().await;
            if guard.as_ref().is_none_or(|s| s.is_closed()) {
                *guard = Some(self.connect().await.map_err(|e| e.to_string())?);
            }
            let req = hyper::Request::get(path)
                .header(hyper::header::HOST, HOST)
                .header(hyper::header::AUTHORIZATION, format!("Bearer {}", self.token))
                .header(hyper::header::USER_AGENT, "tessel")
                .body(Empty::<Bytes>::new())
                .map_err(|e| e.to_string())?;
            let sent = guard.as_mut().expect("just made").send_request(req).await;
            match sent {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry = resp.headers().get(hyper::header::RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse().ok());
                    let body = resp.into_body().collect().await.map_err(|e| e.to_string())?.to_bytes();
                    return Ok((status, retry, body));
                }
                Err(e) => {
                    last = e.to_string();
                    *guard = None;
                }
            }
        }
        Err(last)
    }
}

impl Web {
    async fn team(&self) -> Option<String> {
        self.team
            .get_or_init(|| async {
                let me = self.raw("auth.test", &[]).await.ok()?;
                if me.get("is_enterprise_install").and_then(Value::as_bool) != Some(true) {
                    return None;
                }
                let teams = self.raw("auth.teams.list", &[]).await.ok()?;
                teams.get("teams")?.as_array()?.first()?.get("id")?.as_str().map(str::to_owned)
            })
            .await
            .clone()
    }

    async fn raw(&self, method: &str, params: &[(&str, String)]) -> Result<Value, Fail> {
        let path = format!("/api/{method}?{}", query(params));
        for _ in 0..4 {
            let (status, retry, body) = self.get(&path).await.map_err(|e| Fail::Other(format!("Couldn't reach Slack: {e}")))?;
            if status == 429 {
                tokio::time::sleep(std::time::Duration::from_secs(retry.unwrap_or(5).clamp(1, 30))).await;
                continue;
            }
            let v: Value = serde_json::from_slice(&body).map_err(|_| Fail::Other(format!("Slack answered {status} to {method}")))?;
            return check(v);
        }
        Err(Fail::Other("Slack kept asking tessel to slow down: try again in a minute".into()))
    }
}

impl Api for Web {
    fn call<'a>(&'a self, method: &'a str, params: Params<'a>) -> Answer<'a> {
        Box::pin(async move {
            if NEEDS_TEAM.contains(&method)
                && !params.iter().any(|(k, _)| *k == "team_id")
                && let Some(team) = self.team().await
            {
                let mut with: Vec<(&str, String)> = params.to_vec();
                with.push(("team_id", team));
                return self.raw(method, &with).await;
            }
            let path = format!("/api/{method}?{}", query(params));
            for _ in 0..4 {
                let (status, retry, body) = self.get(&path).await.map_err(|e| Fail::Other(format!("Couldn't reach Slack: {e}")))?;
                if status == 429 {
                    tokio::time::sleep(std::time::Duration::from_secs(retry.unwrap_or(5).clamp(1, 30))).await;
                    continue;
                }
                let v: Value = serde_json::from_slice(&body).map_err(|_| Fail::Other(format!("Slack answered {status} to {method}")))?;
                return check(v);
            }
            Err(Fail::Other("Slack kept asking tessel to slow down: try again in a minute".into()))
        })
    }
}

pub fn query(params: &[(&str, String)]) -> String {
    let enc = |s: &str| {
        let mut out = String::new();
        for b in s.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
                out.push(b as char);
            } else {
                out.push_str(&format!("%{b:02X}"));
            }
        }
        out
    };
    params.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

pub async fn connect(home: PathBuf, stub: Option<Arc<dyn Api>>) -> Result<Arc<dyn Api>, String> {
    if let Some(api) = stub {
        return Ok(api);
    }
    let token = tokio::task::spawn_blocking(move || {
        if !crate::keys::load(&home).iter().any(|k| k.name == KEY) {
            return None;
        }
        crate::keys::secret(&home, KEY)
    })
    .await
    .ok()
    .flatten()
    .map(|t| t.trim().to_owned())
    .filter(|t| !t.is_empty())
    .ok_or_else(|| NOT_CONNECTED.to_owned())?;
    Ok(Arc::new(Web::new(token)?))
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Ledger {
    #[serde(default)]
    pub channels: BTreeMap<String, Read>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Read {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_read_ts: Option<String>,
    #[serde(default)]
    pub used: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub roots: BTreeSet<String>,
}

pub fn ledger_path(home: &Path, repo: &str) -> PathBuf {
    let name: String = repo.trim_matches('/').chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '-' }).collect();
    home.join("slack").join(format!("{}.json", if name.is_empty() { "root".to_owned() } else { name }))
}

pub fn load(path: &Path) -> Ledger {
    std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

pub fn save(path: &Path, ledger: &Ledger) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_vec_pretty(ledger).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(tmp, path)
}

pub fn ts_key(ts: &str) -> (i64, i64) {
    let (s, f) = ts.split_once('.').unwrap_or((ts, ""));
    let f: String = f.chars().chain(std::iter::repeat('0')).take(6).collect();
    (s.parse().unwrap_or(0), f.parse().unwrap_or(0))
}

pub fn newer(a: &str, b: &str) -> bool {
    ts_key(a) > ts_key(b)
}

pub fn since(given: Option<i64>, last_read: Option<&str>, now: i64) -> String {
    match (given, last_read) {
        (Some(s), _) => format!("{s}.000000"),
        (None, Some(ts)) => ts.to_owned(),
        (None, None) => format!("{}.000000", now - 7 * DAY),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Msg {
    pub ts: String,
    pub thread: Option<String>,
    pub author: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shown {
    pub msg: Msg,
    pub follow_up: Option<(String, String)>,
}

pub struct Tasked {
    pub created_secs: i64,
    pub title: String,
}

pub fn sift(messages: Vec<Msg>, read: &Read, tasks: &HashMap<String, Tasked>) -> (Vec<Shown>, u32) {
    let mut shown = Vec::new();
    let mut skipped = 0;
    for m in messages {
        if read.used.contains_key(&m.ts) {
            skipped += 1;
            continue;
        }
        let root_task = m.thread.as_ref().and_then(|root| read.used.get(root));
        match root_task {
            None => shown.push(Shown { msg: m, follow_up: None }),
            Some(task) => match tasks.get(task) {
                Some(t) if ts_key(&m.ts).0 >= t.created_secs => shown.push(Shown { msg: m, follow_up: Some((task.clone(), t.title.clone())) }),
                _ => skipped += 1,
            },
        }
    }
    (shown, skipped)
}

fn date(ts: &str) -> String {
    let l = crate::triggers::Local::at(ts_key(ts).0);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", l.year, l.month, l.day, l.hour, l.minute)
}

fn line(s: &Shown, indent: &str) -> String {
    let m = &s.msg;
    let after = match &s.follow_up {
        Some((id, title)) => format!(" (follow-up on task {id} \"{title}\")"),
        None if m.thread.is_some() && indent.is_empty() => " (reply in a thread)".to_owned(),
        None => String::new(),
    };
    let text = m.text.trim().replace('\n', &format!("\n{indent}    "));
    format!("{indent}[{}] {} {}{after}: {text}", m.ts, date(&m.ts), m.author)
}

pub struct Rendered {
    pub text: String,
    pub included: Vec<Shown>,
    pub cut: bool,
}

pub fn watermark(r: &Rendered, read: &[String]) -> Option<String> {
    let newest = |all: &mut dyn Iterator<Item = &String>| all.max_by_key(|ts| ts_key(ts)).cloned();
    if r.cut { newest(&mut r.included.iter().filter(|s| s.msg.thread.is_none()).map(|s| &s.msg.ts)) } else { newest(&mut read.iter()) }
}

pub fn render(shown: Vec<Shown>, most_chars: usize) -> Rendered {
    let present: BTreeSet<String> = shown.iter().map(|s| s.msg.ts.clone()).collect();
    let mut blocks: Vec<(String, Vec<Shown>)> = Vec::new();
    let mut under: HashMap<String, Vec<Shown>> = HashMap::new();
    for s in shown {
        match &s.msg.thread {
            Some(root) if present.contains(root) => under.entry(root.clone()).or_default().push(s),
            _ => blocks.push((s.msg.ts.clone(), vec![s])),
        }
    }
    for (root, block) in blocks.iter_mut() {
        if let Some(mut replies) = under.remove(root) {
            replies.sort_by_key(|r| ts_key(&r.msg.ts));
            block.extend(replies);
        }
    }
    blocks.sort_by_key(|(ts, _)| ts_key(ts));
    let mut text = String::new();
    let mut included = Vec::new();
    let mut cut = false;
    for (_, block) in blocks {
        let lines: Vec<String> = block.iter().enumerate().map(|(i, s)| line(s, if i == 0 { "" } else { "    " })).collect();
        let piece = lines.join("\n") + "\n";
        if !text.is_empty() && text.len() + piece.len() > most_chars {
            cut = true;
            break;
        }
        text.push_str(&piece);
        included.extend(block);
    }
    Rendered { text, included, cut }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Filed {
    pub title: String,
    pub spec: String,
    pub from: Vec<String>,
}

pub fn parse(text: &str) -> Result<Vec<Filed>, String> {
    let v = crate::proof::json_tail(text).ok_or("The model didn't end with a list of tasks")?;
    let items = match &v {
        Value::Array(a) => a.clone(),
        other => other.get("tasks").and_then(Value::as_array).cloned().ok_or("The model's answer had no tasks list")?,
    };
    let mut out = Vec::new();
    for item in items {
        let s = |k: &str| item.get(k).and_then(Value::as_str).map(str::trim).unwrap_or_default().to_owned();
        let title = s("title");
        if title.is_empty() {
            continue;
        }
        let from = item.get("from").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(|t| t.trim().trim_start_matches('[').trim_end_matches(']').to_owned()).filter(|t| !t.is_empty()).collect()).unwrap_or_default();
        out.push(Filed { title, spec: s("spec"), from });
        if out.len() >= MOST_TASKS {
            break;
        }
    }
    Ok(out)
}

pub fn traced(tasks: Vec<Filed>, shown: &HashMap<String, String>) -> Vec<Filed> {
    tasks
        .into_iter()
        .filter_map(|mut t| {
            let mut seen = BTreeSet::new();
            t.from.retain(|ts| shown.contains_key(ts) && seen.insert(ts.clone()));
            (!t.from.is_empty()).then_some(t)
        })
        .collect()
}

pub fn prompt(repo: &str, existing: &[String], note: &str, messages: &str) -> String {
    let existing = if existing.is_empty() { "(none yet)".to_owned() } else { existing.iter().map(|t| format!("- {t}")).collect::<Vec<_>>().join("\n") };
    let note = if note.trim().is_empty() { "(no note)".to_owned() } else { note.trim().to_owned() };
    format!(
        "You read a team's Slack channel and turn its requests, bugs and decisions into tasks for coding agents working in the repository {repo}.\n\n\
Each task has a short imperative title, a spec that says what done looks like and quotes the messages it comes from, and `from`: the ids of the messages it came from (the number in square brackets at the start of each message).\n\n\
Merge messages about the same thing into one task. Skip chatter, questions that were already answered, things that aren't changes to this repository, and anything one of these existing tasks already covers:\n{existing}\n\n\
A message marked as a follow-up on a task is about work that's already filed: only make a new task of it when it asks for something that task doesn't cover.\n\n\
The person asking says: {note}\n\
Follow that if it's there: it can narrow things by topic, by person or by date. Every message shows its date and its author.\n\n\
The messages, oldest first, as `[id] date time author: text`, with replies in a thread indented under the message they answer:\n\n{messages}\n\
End your answer with one ```json block and nothing after it, like this:\n\
```json\n{{\"tasks\": [{{\"title\": \"…\", \"spec\": \"…\", \"from\": [\"<id>\"]}}]}}\n```\n\
If nothing in the channel should become a task, answer with {{\"tasks\": []}}."
    )
}

pub fn looks_like_id(s: &str) -> bool {
    s.len() >= 9 && matches!(s.as_bytes()[0], b'C' | b'G') && s.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

pub fn not_in(name: &str) -> String {
    format!("The bot isn't in #{name}: invite it with /invite @YourApp")
}

pub fn pick(all: &[(SlackChannel, bool)], wanted: &str) -> Result<SlackChannel, String> {
    let name = wanted.trim().trim_start_matches('#');
    let found = all.iter().find(|(c, _)| c.id == name).or_else(|| all.iter().find(|(c, _)| c.name.eq_ignore_ascii_case(name)));
    match found {
        Some((c, true)) => Ok(c.clone()),
        Some((c, false)) => Err(not_in(&c.name)),
        None => Err(format!("Slack has no channel #{name} that the bot can see: if it's private, invite the bot with /invite @YourApp")),
    }
}

fn channel_of(c: &Value) -> Option<(SlackChannel, bool)> {
    let id = c.get("id")?.as_str()?.to_owned();
    let name = c.get("name").and_then(Value::as_str).unwrap_or(&id).to_owned();
    let private = c.get("is_private").and_then(Value::as_bool).unwrap_or(false) || c.get("is_group").and_then(Value::as_bool).unwrap_or(false);
    let member = c.get("is_member").and_then(Value::as_bool).unwrap_or(private);
    Some((SlackChannel { id, name, private }, member))
}

fn cursor(v: &Value) -> Option<String> {
    v.pointer("/response_metadata/next_cursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_owned)
}

pub async fn all_channels(api: &dyn Api) -> Result<Vec<(SlackChannel, bool)>, Fail> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MOST_PAGES {
        let mut p = vec![("types", "public_channel,private_channel".to_owned()), ("exclude_archived", "true".to_owned()), ("limit", "200".to_owned())];
        if let Some(c) = &next {
            p.push(("cursor", c.clone()));
        }
        let v = api.call("conversations.list", &p).await?;
        out.extend(v.get("channels").and_then(Value::as_array).into_iter().flatten().filter_map(channel_of));
        next = cursor(&v);
        if next.is_none() {
            break;
        }
    }
    Ok(out)
}

async fn member_channels(api: &dyn Api) -> Result<Vec<SlackChannel>, Fail> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MOST_PAGES {
        let mut p = vec![("types", "public_channel,private_channel".to_owned()), ("exclude_archived", "true".to_owned()), ("limit", "200".to_owned())];
        if let Some(c) = &next {
            p.push(("cursor", c.clone()));
        }
        let v = api.call("users.conversations", &p).await?;
        out.extend(v.get("channels").and_then(Value::as_array).into_iter().flatten().filter_map(channel_of).map(|(c, _)| c));
        next = cursor(&v);
        if next.is_none() {
            break;
        }
    }
    Ok(out)
}

pub async fn channels(api: &dyn Api) -> Result<Vec<SlackChannel>, String> {
    let mut list: Vec<SlackChannel> = match member_channels(api).await {
        Ok(list) => list,
        Err(_) => all_channels(api).await.map_err(|e| e.sentence())?.into_iter().filter(|(_, member)| *member).map(|(c, _)| c).collect(),
    };
    list.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(list)
}

pub async fn resolve(api: &dyn Api, wanted: &str) -> Result<SlackChannel, String> {
    let name = wanted.trim().trim_start_matches('#');
    if name.is_empty() {
        return Err("Say which channel to read".into());
    }
    if looks_like_id(name) {
        return match api.call("conversations.info", &[("channel", name.to_owned())]).await {
            Ok(v) => match v.get("channel").and_then(channel_of) {
                Some((c, true)) => Ok(c),
                Some((c, false)) => Err(not_in(&c.name)),
                None => Err(format!("Slack has no channel {name}")),
            },
            Err(Fail::Said(code)) if code == "channel_not_found" => Err(format!("Slack has no channel {name} that the bot can see: if it's private, invite the bot with /invite @YourApp")),
            Err(e) => Err(e.sentence()),
        };
    }
    if let Ok(mine) = member_channels(api).await
        && let Some(c) = mine.into_iter().find(|c| c.name == name)
    {
        return Ok(c);
    }
    pick(&all_channels(api).await.map_err(|e| e.sentence())?, name)
}

fn kept(m: &Value) -> bool {
    if m.get("bot_id").is_some() || m.get("user").and_then(Value::as_str).is_none() {
        return false;
    }
    match m.get("subtype").and_then(Value::as_str) {
        None | Some("thread_broadcast" | "file_share" | "me_message") => {}
        Some(_) => return false,
    }
    !text_of(m).trim().is_empty()
}

fn text_of(m: &Value) -> String {
    let mut text = m.get("text").and_then(Value::as_str).unwrap_or_default().to_owned();
    for f in m.get("files").and_then(Value::as_array).into_iter().flatten() {
        if let Some(name) = f.get("name").or_else(|| f.get("title")).and_then(Value::as_str) {
            text.push_str(&format!(" [file: {name}]"));
        }
    }
    text
}

fn raw(m: &Value, thread: Option<String>) -> Option<(Msg, String)> {
    if !kept(m) {
        return None;
    }
    let ts = m.get("ts")?.as_str()?.to_owned();
    let user = m.get("user")?.as_str()?.to_owned();
    let thread = thread.filter(|t| *t != ts);
    Some((Msg { ts, thread, author: String::new(), text: text_of(m) }, user))
}

fn mentions(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"<@([UW][A-Z0-9]+)(?:\|[^>]*)?>").expect("a pattern");
    re.captures_iter(text).map(|c| c[1].to_owned()).collect()
}

pub fn tidy(text: &str, names: &HashMap<String, String>) -> String {
    let user = regex::Regex::new(r"<@([UW][A-Z0-9]+)(?:\|([^>]*))?>").expect("a pattern");
    let text = user.replace_all(text, |c: &regex::Captures| format!("@{}", names.get(&c[1]).cloned().or_else(|| c.get(2).map(|m| m.as_str().to_owned())).unwrap_or_else(|| c[1].to_owned())));
    let chan = regex::Regex::new(r"<#[A-Z0-9]+\|([^>]*)>").expect("a pattern");
    let text = chan.replace_all(&text, "#$1");
    let special = regex::Regex::new(r"<!(here|channel|everyone)[^>]*>").expect("a pattern");
    let text = special.replace_all(&text, "@$1");
    let link = regex::Regex::new(r"<((?:https?|mailto):[^|>]+)\|([^>]+)>").expect("a pattern");
    let text = link.replace_all(&text, "$2 ($1)");
    let bare = regex::Regex::new(r"<((?:https?|mailto):[^>]+)>").expect("a pattern");
    let text = bare.replace_all(&text, "$1");
    text.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")
}

async fn pages(api: &dyn Api, method: &str, base: Vec<(&str, String)>) -> Result<Vec<Value>, Fail> {
    let mut out = Vec::new();
    let mut next: Option<String> = None;
    for _ in 0..MOST_PAGES {
        let mut p = base.clone();
        if let Some(c) = &next {
            p.push(("cursor", c.clone()));
        }
        let v = api.call(method, &p).await?;
        out.extend(v.get("messages").and_then(Value::as_array).into_iter().flatten().cloned());
        next = cursor(&v);
        if next.is_none() || v.get("has_more").and_then(Value::as_bool) == Some(false) {
            break;
        }
    }
    Ok(out)
}

pub fn permalink(team_url: &str, channel: &str, ts: &str, thread: Option<&str>) -> String {
    let base = format!("{}/archives/{channel}/p{}", team_url.trim_end_matches('/'), ts.replace('.', ""));
    match thread {
        Some(root) => format!("{base}?thread_ts={root}&cid={channel}"),
        None => base,
    }
}

pub struct Ask {
    pub repo: String,
    pub channel: String,
    pub since: Option<i64>,
    pub note: String,
    pub ledger: Ledger,
    pub tasks: HashMap<String, Tasked>,
    pub existing: Vec<String>,
    pub model: String,
    pub now: i64,
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub channel: SlackChannel,
    pub read: u32,
    pub skipped: u32,
    pub watermark: Option<String>,
    pub tasks: Vec<Filed>,
    pub links: HashMap<String, String>,
    pub roots: BTreeSet<String>,
}

pub async fn read(api: Arc<dyn Api>, backend: Arc<dyn Backend>, ask: Ask) -> Result<Outcome, String> {
    let api = api.as_ref();
    let team_url = api.call("auth.test", &[]).await.map_err(|e| e.sentence())?.get("url").and_then(Value::as_str).unwrap_or("https://slack.com/").to_owned();
    let channel = resolve(api, &ask.channel).await?;
    let said = |e: Fail| match &e {
        Fail::Said(code) if code == "not_in_channel" => not_in(&channel.name),
        _ => e.sentence(),
    };
    let before = ask.ledger.channels.get(&channel.id).cloned().unwrap_or_default();
    let oldest = since(ask.since, before.last_read_ts.as_deref(), ask.now);
    let history = pages(api, "conversations.history", vec![("channel", channel.id.clone()), ("oldest", oldest.clone()), ("limit", "200".to_owned())]).await.map_err(said)?;
    let mut found: BTreeMap<String, (Msg, String)> = BTreeMap::new();
    let mut threads: Vec<String> = Vec::new();
    for m in &history {
        let ts = m.get("ts").and_then(Value::as_str).unwrap_or_default().to_owned();
        let thread = m.get("thread_ts").and_then(Value::as_str).map(str::to_owned);
        if m.get("reply_count").and_then(Value::as_u64).unwrap_or(0) > 0 && thread.as_deref().is_none_or(|t| t == ts) {
            threads.push(ts.clone());
        }
        if let Some(r) = raw(m, thread) {
            found.insert(r.0.ts.clone(), r);
        }
    }
    let recent = ask.now - FOLLOW_UP_DAYS * DAY;
    let mut old_roots: Vec<&String> = before.roots.iter().filter(|r| !threads.contains(r) && !found.contains_key(*r) && ts_key(r).0 >= recent).collect();
    old_roots.sort_by_key(|r| std::cmp::Reverse(ts_key(r)));
    old_roots.truncate(MOST_FOLLOW_UP_THREADS);
    let mut asks: Vec<(String, Option<String>)> = threads.iter().map(|t| (t.clone(), None)).collect();
    asks.extend(old_roots.into_iter().map(|r| (r.clone(), Some(oldest.clone()))));
    for (root, from) in asks {
        let mut p = vec![("channel", channel.id.clone()), ("ts", root.clone()), ("limit", "200".to_owned())];
        if let Some(o) = &from {
            p.push(("oldest", o.clone()));
        }
        let replies = match pages(api, "conversations.replies", p).await {
            Ok(r) => r,
            Err(_) if from.is_some() => continue,
            Err(e) => return Err(said(e)),
        };
        for m in &replies {
            if let Some(r) = raw(m, Some(root.clone()))
                && r.0.thread.is_some()
                && (from.is_none() || newer(&r.0.ts, &oldest))
            {
                found.insert(r.0.ts.clone(), r);
            }
        }
    }
    let mut people: BTreeSet<String> = found.values().map(|(_, u)| u.clone()).collect();
    people.extend(found.values().flat_map(|(m, _)| mentions(&m.text)));
    let mut names: HashMap<String, String> = HashMap::new();
    for id in people {
        let name = match api.call("users.info", &[("user", id.clone())]).await {
            Ok(v) => {
                let s = |p: &str| v.pointer(p).and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
                s("/user/profile/display_name").or_else(|| s("/user/real_name")).or_else(|| s("/user/name")).unwrap_or_else(|| id.clone())
            }
            Err(_) => id.clone(),
        };
        names.insert(id, name);
    }
    let messages: Vec<Msg> = found
        .into_values()
        .map(|(mut m, user)| {
            m.author = names.get(&user).cloned().unwrap_or(user);
            m.text = tidy(&m.text, &names);
            m
        })
        .collect();
    let every: Vec<String> = messages.iter().map(|m| m.ts.clone()).collect();
    let (shown, skipped) = sift(messages, &before, &ask.tasks);
    let rendered = render(shown, MOST_CHARS);
    let read = rendered.included.len() as u32 + skipped;
    let links: HashMap<String, String> = rendered.included.iter().map(|s| (s.msg.ts.clone(), permalink(&team_url, &channel.id, &s.msg.ts, s.msg.thread.as_deref()))).collect();
    let roots: BTreeSet<String> = rendered.included.iter().filter(|s| s.msg.thread.is_none()).map(|s| s.msg.ts.clone()).collect();
    let watermark = watermark(&rendered, &every);
    if rendered.included.is_empty() {
        return Ok(Outcome { channel, read, skipped, watermark, tasks: Vec::new(), links, roots });
    }
    let repo_name = Path::new(&ask.repo).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| ask.repo.clone());
    let mut job = Job::review(prompt(&repo_name, &ask.existing, &ask.note, &rendered.text), PathBuf::from(&ask.repo), &ask.model);
    job.tools = String::new();
    job.system = "You turn a team's chat into clear tasks for coding agents. Be precise and brief, and never invent work nobody asked for.".into();
    job.minutes = 5;
    let answer = backend.job(job).await.map_err(|e| format!("The model couldn't read the channel: {e:#}"))?;
    let tasks = traced(parse(&answer.text)?, &links);
    Ok(Outcome { channel, read, skipped, watermark, tasks, links, roots })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msg(ts: &str, thread: Option<&str>, text: &str) -> Msg {
        Msg { ts: ts.into(), thread: thread.map(str::to_owned), author: "ana".into(), text: text.into() }
    }

    #[test]
    fn messages_already_filed_are_taken_out_and_counted() {
        let mut read = Read::default();
        read.used.insert("100.000001".into(), "t-1".into());
        let tasks = HashMap::from([("t-1".to_owned(), Tasked { created_secs: 150, title: "Fix login".into() })]);
        let (shown, skipped) = sift(vec![msg("100.000001", None, "login is broken"), msg("101.000000", None, "dark mode please")], &read, &tasks);
        assert_eq!(skipped, 1);
        assert_eq!(shown.iter().map(|s| s.msg.ts.as_str()).collect::<Vec<_>>(), vec!["101.000000"]);
    }

    #[test]
    fn replies_to_a_filed_message_are_skipped_unless_they_came_after_its_task() {
        let mut read = Read::default();
        read.used.insert("100.000001".into(), "t-1".into());
        let tasks = HashMap::from([("t-1".to_owned(), Tasked { created_secs: 150, title: "Fix login".into() })]);
        let (shown, skipped) = sift(
            vec![msg("120.000000", Some("100.000001"), "same here"), msg("200.000000", Some("100.000001"), "still broken on safari"), msg("130.000000", Some("90.000000"), "an unrelated thread")],
            &read,
            &tasks,
        );
        assert_eq!(skipped, 1, "the reply from before the task was filed is part of it");
        assert_eq!(shown.len(), 2);
        let later = shown.iter().find(|s| s.msg.ts == "200.000000").unwrap();
        assert_eq!(later.follow_up, Some(("t-1".into(), "Fix login".into())));
        assert!(shown.iter().find(|s| s.msg.ts == "130.000000").unwrap().follow_up.is_none());
        let text = render(shown, 10_000).text;
        assert!(text.contains("(follow-up on task t-1 \"Fix login\")"), "{text}");
    }

    #[test]
    fn replies_of_an_unknown_task_s_message_are_skipped() {
        let mut read = Read::default();
        read.used.insert("100.000001".into(), "t-gone".into());
        let (shown, skipped) = sift(vec![msg("300.000000", Some("100.000001"), "hm")], &read, &HashMap::new());
        assert!(shown.is_empty());
        assert_eq!(skipped, 1);
    }

    #[test]
    fn a_read_starts_where_the_last_one_ended_or_a_week_ago() {
        let now = 1_000_000;
        assert_eq!(since(None, None, now), format!("{}.000000", now - 7 * 86_400));
        assert_eq!(since(None, Some("999000.000300"), now), "999000.000300");
        assert_eq!(since(Some(500), Some("999000.000300"), now), "500.000000", "asked for, it wins");
    }

    #[test]
    fn slack_times_compare_as_numbers() {
        assert!(newer("1700000000.000100", "1700000000.000099"));
        assert!(newer("1700000001.0", "1700000000.999999"));
        assert!(!newer("1700000000.000100", "1700000000.0001"));
    }

    #[test]
    fn the_model_s_tasks_are_capped_and_need_titles() {
        let mut tasks: Vec<Value> = (0..20).map(|i| serde_json::json!({"title": format!("Task {i}"), "spec": "s", "from": [format!("[{i}.000000]")]})).collect();
        tasks.insert(0, serde_json::json!({"title": "  ", "spec": "nothing", "from": ["1.0"]}));
        let text = format!("Here they are.\n```json\n{}\n```", serde_json::json!({ "tasks": tasks }));
        let got = parse(&text).unwrap();
        assert_eq!(got.len(), MOST_TASKS);
        assert_eq!(got[0], Filed { title: "Task 0".into(), spec: "s".into(), from: vec!["0.000000".into()] });
        assert!(parse("```json\n{\"tasks\": []}\n```").unwrap().is_empty());
        assert!(parse("no idea").is_err());
    }

    #[test]
    fn a_task_must_come_from_messages_the_model_was_shown() {
        let shown = HashMap::from([("1.000000".to_owned(), "link1".to_owned()), ("2.000000".to_owned(), "link2".to_owned())]);
        let t = |title: &str, from: &[&str]| Filed { title: title.into(), spec: String::new(), from: from.iter().map(|s| (*s).to_owned()).collect() };
        let kept = traced(vec![t("a", &["2.000000", "9.000000", "2.000000", "1.000000"]), t("b", &["9.000000"]), t("c", &[])], &shown);
        assert_eq!(kept, vec![t("a", &["2.000000", "1.000000"])], "made up or already filed messages file nothing");
    }

    #[test]
    fn a_channel_is_found_by_id_or_name_with_or_without_its_hash() {
        let all = vec![
            (SlackChannel { id: "C0001AAAA".into(), name: "shop-bugs".into(), private: false }, true),
            (SlackChannel { id: "C0002BBBB".into(), name: "random".into(), private: false }, false),
        ];
        assert_eq!(pick(&all, "#shop-bugs").unwrap().id, "C0001AAAA");
        assert_eq!(pick(&all, "Shop-Bugs").unwrap().id, "C0001AAAA");
        assert_eq!(pick(&all, "C0001AAAA").unwrap().name, "shop-bugs");
        assert_eq!(pick(&all, "random").unwrap_err(), "The bot isn't in #random: invite it with /invite @YourApp");
        assert!(pick(&all, "#nope").unwrap_err().starts_with("Slack has no channel #nope"));
        assert!(looks_like_id("C0001AAAA") && looks_like_id("G01234ABC") && !looks_like_id("general") && !looks_like_id("Cats"));
    }

    #[test]
    fn replies_sit_under_their_message_and_a_cut_keeps_the_oldest() {
        let shown = |ts: &str, thread: Option<&str>| Shown { msg: msg(ts, thread, &format!("m{ts}")), follow_up: None };
        let r = render(vec![shown("300.0", None), shown("100.0", None), shown("150.0", Some("100.0")), shown("250.0", Some("50.0"))], 10_000);
        let order: Vec<&str> = r.text.lines().map(|l| l.split(']').next().unwrap().trim()).collect();
        assert_eq!(order, vec!["[100.0", "[150.0", "[250.0", "[300.0"]);
        assert!(r.text.lines().nth(1).unwrap().starts_with("    ["), "{}", r.text);
        assert!(r.text.contains("(reply in a thread)"));
        assert!(!r.cut);
        assert_eq!(watermark(&r, &["300.0".into(), "100.0".into(), "350.0".into()]).as_deref(), Some("350.0"), "messages skipped as filed count as read");
        let first = r.text.lines().take(2).map(|l| l.len() + 1).sum::<usize>();
        let cut = render(vec![shown("300.0", None), shown("100.0", None), shown("150.0", Some("100.0")), shown("250.0", Some("50.0"))], first + 1);
        assert_eq!(cut.included.len(), 2);
        assert!(cut.cut);
        assert_eq!(watermark(&cut, &["300.0".into(), "100.0".into()]).as_deref(), Some("100.0"), "what wasn't read is read next time");
    }

    #[test]
    fn slack_markup_reads_as_plain_text() {
        let names = HashMap::from([("U1".to_owned(), "ana".to_owned())]);
        assert_eq!(tidy("<@U1> see <https://x.dev/a|the page> in <#C1|bugs> &amp; <!here>", &names), "@ana see the page (https://x.dev/a) in #bugs & @here");
        assert_eq!(permalink("https://acme.slack.com/", "C1", "1700000000.000100", None), "https://acme.slack.com/archives/C1/p1700000000000100");
        assert_eq!(permalink("https://acme.slack.com/", "C1", "1700000001.000200", Some("1700000000.000100")), "https://acme.slack.com/archives/C1/p1700000001000200?thread_ts=1700000000.000100&cid=C1");
        assert_eq!(query(&[("oldest", "1.5".into()), ("types", "a,b".into())]), "oldest=1.5&types=a%2Cb");
    }

    #[test]
    fn the_ledger_lives_under_slack_named_for_the_repo() {
        let p = ledger_path(Path::new("/h"), "/Users/me/code/shop");
        assert_eq!(p, PathBuf::from("/h/slack/Users-me-code-shop.json"));
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("slack/x.json");
        let mut l = Ledger::default();
        l.channels.entry("C1".into()).or_default().used.insert("1.0".into(), "t-1".into());
        save(&path, &l).unwrap();
        assert_eq!(load(&path), l);
    }
}
