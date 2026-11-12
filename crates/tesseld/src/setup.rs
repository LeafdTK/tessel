//! what a workspace needs to be worked in from its first minute: the
//! gitignored files the repository's checkout has (`.env`), its setup (install
//! the dependencies), buttons for what you run in it (the dev server, the
//! tests), and ports of its own so two workspaces can both run a server.
//!
//! a repository says all this in `.tessel/settings.toml`. one set up for
//! Conductor works as it is: its `.conductor/settings.toml` (or the older
//! `conductor.json`) is read when there's no tessel file, and every script also
//! sees the `CONDUCTOR_` names for what it's given.
//!
//! ```toml
//! [scripts]
//! setup = "pnpm install"
//!
//! [scripts.run.dev]
//! command = "pnpm dev --port $TESSEL_PORT"
//! default = true
//!
//! [scripts.run.test]
//! command = "pnpm test --watch"
//! icon = "test-tube"
//!
//! [files]
//! copy = [".env*", "config/local.json"]
//! ```

use globset::{Glob, GlobSetBuilder};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tessel_proto::{ScriptAction, WorkspaceScripts};
use toml::Value;

/// the files that can say what a workspace needs, least important first: a
/// later one's keys win.
const SOURCES: [&str; 5] = ["conductor.json", ".conductor/settings.toml", ".conductor/settings.local.toml", ".tessel/settings.toml", ".tessel/settings.local.toml"];

/// what's copied into a new workspace when nothing says otherwise.
pub const DEFAULT_COPY: &str = ".env*";

/// read what a workspace needs: from the workspace itself, else from the
/// repository's own checkout (a settings file not committed yet lives there).
pub fn load(workspace: &Path, root: &Path) -> WorkspaceScripts {
    let found = read_from(workspace);
    let mut scripts = if found.from.is_some() || workspace == root { found } else { read_from(root) };
    if scripts.copy.is_empty() {
        scripts.copy = worktreeinclude(root).unwrap_or_else(|| vec![DEFAULT_COPY.to_owned()]);
    }
    scripts
}

fn read_from(dir: &Path) -> WorkspaceScripts {
    let mut merged = toml::Table::new();
    let mut from = None;
    for name in SOURCES {
        let Ok(text) = std::fs::read_to_string(dir.join(name)) else { continue };
        let table = if name.ends_with(".json") {
            serde_json::from_str::<serde_json::Value>(&text).ok().and_then(|v| json_to_toml(&v)).and_then(|v| v.as_table().cloned())
        } else {
            text.parse::<toml::Table>().ok()
        };
        if let Some(table) = table {
            merge(&mut merged, table);
            from = Some(name.to_owned());
        }
    }
    let mut scripts = parse(&merged);
    scripts.from = from;
    scripts
}

/// a later table's keys over an earlier one's, tables merged key by key.
fn merge(into: &mut toml::Table, from: toml::Table) {
    for (k, v) in from {
        match (into.get_mut(&k), v) {
            (Some(Value::Table(a)), Value::Table(b)) => merge(a, b),
            (_, v) => {
                into.insert(k, v);
            }
        }
    }
}

fn json_to_toml(v: &serde_json::Value) -> Option<Value> {
    Some(match v {
        serde_json::Value::String(s) => Value::String(s.clone()),
        serde_json::Value::Bool(b) => Value::Boolean(*b),
        serde_json::Value::Number(n) => n.as_i64().map(Value::Integer).or_else(|| n.as_f64().map(Value::Float))?,
        serde_json::Value::Array(a) => Value::Array(a.iter().filter_map(json_to_toml).collect()),
        serde_json::Value::Object(o) => Value::Table(o.iter().filter_map(|(k, v)| Some((k.clone(), json_to_toml(v)?))).collect()),
        serde_json::Value::Null => return None,
    })
}

/// the settings, in either spelling: tessel's, or Conductor's (`file_include_globs`,
/// `runScript`, a single `scripts.run` string).
pub fn parse(t: &toml::Table) -> WorkspaceScripts {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let scripts = t.get("scripts").and_then(Value::as_table);
    let mut out = WorkspaceScripts {
        setup: text(scripts.and_then(|s| s.get("setup"))),
        archive: text(scripts.and_then(|s| s.get("archive"))),
        ..WorkspaceScripts::default()
    };
    match scripts.and_then(|s| s.get("run")) {
        Some(Value::String(cmd)) if !cmd.trim().is_empty() => out.actions.push(ScriptAction { id: "run".into(), command: cmd.trim().into(), icon: None, default: true }),
        Some(Value::Table(run)) => {
            // in the file's order would be nicer, but toml tables are sorted; name order is stable.
            let ordered: BTreeMap<&String, &Value> = run.iter().collect();
            for (id, a) in ordered {
                let (command, icon, default) = match a {
                    Value::String(c) => (Some(c.trim().to_owned()), None, false),
                    Value::Table(a) => (text(a.get("command")), text(a.get("icon")), a.get("default").and_then(Value::as_bool).unwrap_or(false)),
                    _ => (None, None, false),
                };
                // an action only for Conductor's cloud has no business here.
                let local = match a.get("available_in").and_then(Value::as_array) {
                    Some(where_) => where_.iter().any(|w| w.as_str() == Some("local")),
                    None => true,
                };
                if let Some(command) = command.filter(|c| !c.is_empty())
                    && local
                {
                    out.actions.push(ScriptAction { id: id.clone(), command, icon, default });
                }
            }
        }
        _ => {}
    }
    if !out.actions.iter().any(|a| a.default)
        && let Some(first) = out.actions.first_mut()
    {
        first.default = true;
    }
    let globs = |v: Option<&Value>| -> Vec<String> { v.and_then(Value::as_array).map(|a| a.iter().filter_map(|g| g.as_str().map(str::to_owned)).collect()).unwrap_or_default() };
    out.copy = globs(t.get("files").and_then(Value::as_table).and_then(|f| f.get("copy")));
    if out.copy.is_empty() {
        out.copy = globs(t.get("file_include_globs"));
    }
    out
}

/// `.worktreeinclude`: one pattern a line, `#` for comments.
fn worktreeinclude(root: &Path) -> Option<Vec<String>> {
    let text = std::fs::read_to_string(root.join(".worktreeinclude")).ok()?;
    let lines: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(str::to_owned).collect();
    (!lines.is_empty()).then_some(lines)
}

/// copy the checkout's gitignored files that match `patterns` into a new
/// workspace. only ignored files: anything tracked is in the worktree already.
/// a pattern without a `/` matches at any depth (`.env*` finds `apps/web/.env`).
/// what was copied, relative to the workspace.
pub async fn copy_files(root: &Path, workspace: &Path, patterns: &[String]) -> Vec<String> {
    let mut set = GlobSetBuilder::new();
    for p in patterns {
        let p = p.trim().trim_start_matches("./").trim_end_matches('/');
        let p = if p.contains('/') { p.to_owned() } else { format!("**/{p}") };
        if let Ok(g) = Glob::new(&p) {
            set.add(g);
        }
    }
    let Ok(set) = set.build() else { return Vec::new() };
    // ignored files, with ignored folders as one line each (node_modules isn't listed file by file).
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--others", "--ignored", "--exclude-standard", "--directory"])
        .stdin(std::process::Stdio::null())
        .output()
        .await;
    let Ok(out) = out else { return Vec::new() };
    let mut copied = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let rel = line.trim_end_matches('/');
        if rel.is_empty() || !set.is_match(rel) {
            continue;
        }
        let (from, to) = (root.join(rel), workspace.join(rel));
        if to.exists() {
            continue;
        }
        if copy_path(&from, &to).is_ok() {
            copied.push(rel.to_owned());
        }
    }
    copied
}

fn copy_path(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if from.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_path(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        std::fs::copy(from, to).map(drop)
    }
}

/// each workspace's ten ports, kept in `ports.json` so a workspace keeps its
/// ports across restarts (a bookmarked `localhost:41020` keeps working).
pub struct Ports {
    path: PathBuf,
    taken: BTreeMap<String, u16>,
}

/// where the blocks start, and how many ports each has.
const FIRST: u16 = 41000;
const BLOCK: u16 = 10;

impl Ports {
    pub fn load(path: PathBuf) -> Self {
        let taken = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Self { path, taken }
    }

    /// the first of a workspace's ports, given one the first time it's asked about.
    pub fn of(&mut self, workspace: &str) -> u16 {
        if let Some(p) = self.taken.get(workspace) {
            return *p;
        }
        let used: std::collections::BTreeSet<u16> = self.taken.values().copied().collect();
        let port = (0..).map(|n: u16| FIRST + n * BLOCK).find(|p| !used.contains(p)).unwrap_or(FIRST);
        self.taken.insert(workspace.to_owned(), port);
        self.save();
        port
    }

    /// a workspace removed gives its ports back.
    pub fn release(&mut self, workspace: &str) {
        if self.taken.remove(workspace).is_some() {
            self.save();
        }
    }

    fn save(&self) {
        if let Ok(text) = serde_json::to_string_pretty(&self.taken) {
            let _ = std::fs::write(&self.path, text);
        }
    }
}

/// what every process in a workspace is told about it: the agent, its
/// terminals, its scripts. Conductor's names too, so its scripts run unchanged.
pub fn env(workspace: &str, root: &str, port: u16) -> Vec<(String, String)> {
    let name = Path::new(workspace).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut out = Vec::new();
    for prefix in ["TESSEL", "CONDUCTOR"] {
        out.push((format!("{prefix}_PORT"), port.to_string()));
        out.push((format!("{prefix}_WORKSPACE_NAME"), name.clone()));
        out.push((format!("{prefix}_WORKSPACE_PATH"), workspace.to_owned()));
        out.push((format!("{prefix}_ROOT_PATH"), root.to_owned()));
    }
    out.push(("CONDUCTOR_IS_LOCAL".into(), "1".into()));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(t: &str) -> toml::Table {
        t.parse().unwrap()
    }

    #[test]
    fn tessels_file_and_conductors_say_the_same_things() {
        let s = parse(&table(
            r#"
            [scripts]
            setup = "pnpm install"
            [scripts.run.dev]
            command = "pnpm dev --port $TESSEL_PORT"
            [scripts.run.test]
            command = "pnpm test"
            icon = "test-tube"
            default = true
            [scripts.run.deploy]
            command = "make deploy"
            available_in = ["cloud"]
            [files]
            copy = [".env*", "config/local.json"]
            "#,
        ));
        assert_eq!(s.setup.as_deref(), Some("pnpm install"));
        assert_eq!(s.actions.iter().map(|a| (a.id.as_str(), a.default)).collect::<Vec<_>>(), [("dev", false), ("test", true)], "cloud-only actions left out");
        assert_eq!(s.copy, [".env*", "config/local.json"]);
        let old = parse(&table(r#"file_include_globs = [".env"]
            [scripts]
            run = "npm start""#));
        assert_eq!(old.actions.iter().map(|a| (a.id.as_str(), a.command.as_str(), a.default)).collect::<Vec<_>>(), [("run", "npm start", true)], "a lone run script is the default");
        assert_eq!(old.copy, [".env"]);
    }

    #[test]
    fn the_nearest_file_wins_and_a_repo_set_up_for_conductor_just_works() {
        let dir = std::env::temp_dir().join(format!("tessel-setup-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join(".conductor")).unwrap();
        std::fs::write(dir.join(".conductor/settings.toml"), "[scripts]\nsetup = \"npm ci\"\n[scripts.run.web]\ncommand = \"npm run dev\"\n").unwrap();
        let s = load(&dir, &dir);
        assert_eq!((s.setup.as_deref(), s.from.as_deref(), s.copy.clone()), (Some("npm ci"), Some(".conductor/settings.toml"), vec![DEFAULT_COPY.to_owned()]));
        std::fs::create_dir_all(dir.join(".tessel")).unwrap();
        std::fs::write(dir.join(".tessel/settings.toml"), "[scripts]\nsetup = \"pnpm install\"\n").unwrap();
        let s = load(&dir, &dir);
        assert_eq!(s.setup.as_deref(), Some("pnpm install"), "tessel's own file wins");
        assert_eq!(s.actions.len(), 1, "and keeps what it doesn't say from the other");
        std::fs::write(dir.join(".worktreeinclude"), "# secrets\n.env.local\n").unwrap();
        assert_eq!(load(&dir, &dir).copy, [".env.local"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_workspace_gets_ports_of_its_own_and_keeps_them() {
        let path = std::env::temp_dir().join(format!("tessel-ports-{}.json", uuid::Uuid::new_v4()));
        let mut ports = Ports::load(path.clone());
        let (a, b) = (ports.of("/w/a"), ports.of("/w/b"));
        assert_eq!((a, b), (FIRST, FIRST + BLOCK));
        assert_eq!(Ports::load(path.clone()).of("/w/a"), a, "across restarts");
        ports.release("/w/a");
        assert_eq!(ports.of("/w/c"), FIRST, "a freed block is used again");
        let env = env("/w/b", "/repo", b);
        assert!(env.contains(&("TESSEL_PORT".into(), "41010".into())) && env.contains(&("CONDUCTOR_PORT".into(), "41010".into())));
        assert!(env.contains(&("TESSEL_WORKSPACE_NAME".into(), "b".into())));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn a_new_workspace_gets_the_checkouts_ignored_files_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("tessel-copy-{}", uuid::Uuid::new_v4()));
        let ws = root.with_extension("ws");
        std::fs::create_dir_all(root.join("apps/web")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/x")).unwrap();
        let git = |args: &[&str]| std::process::Command::new("git").arg("-C").arg(&root).args(args).output().unwrap();
        git(&["init", "-q"]);
        std::fs::write(root.join(".gitignore"), ".env*\nnode_modules/\n").unwrap();
        std::fs::write(root.join(".env"), "A=1").unwrap();
        std::fs::write(root.join("apps/web/.env.local"), "B=2").unwrap();
        std::fs::write(root.join("node_modules/x/index.js"), "").unwrap();
        std::fs::create_dir_all(&ws).unwrap();
        let mut copied = copy_files(&root, &ws, &[DEFAULT_COPY.to_owned()]).await;
        copied.sort();
        assert_eq!(copied, [".env", "apps/web/.env.local"]);
        assert_eq!(std::fs::read_to_string(ws.join("apps/web/.env.local")).unwrap(), "B=2");
        assert!(!ws.join("node_modules").exists(), "ignored, but not asked for");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&ws);
    }
}
