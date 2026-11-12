//! what a workspace needs to be worked in from its first minute: the
//! gitignored files the repository's checkout has (`.env`), its setup (install
//! the dependencies), buttons for what you run in it (the dev server, the
//! tests), its archive script for when it's removed, and ports of its own so
//! two workspaces can both run a server.
//!
//! a repository says all this in `.tessel/settings.toml`, committed so the
//! whole team has it, and `.tessel/settings.local.toml` in the main checkout
//! for what only this machine does. the local file wins.
//!
//! ```toml
//! [scripts]
//! setup = "pnpm install"
//! archive = "docker compose down"
//! run_mode = "concurrent"        # or "nonconcurrent": one workspace at a time
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

pub const SHARED: &str = ".tessel/settings.toml";
pub const LOCAL: &str = ".tessel/settings.local.toml";

/// what's copied into a new workspace when nothing says otherwise.
pub const DEFAULT_COPY: &str = ".env*";

/// the files that say what a workspace needs, least important first: a
/// later one's keys win. the workspace's shared file is newer than the main
/// checkout's when its branch changed it; this machine's file beats both.
fn sources(workspace: &Path, root: &Path) -> Vec<PathBuf> {
    let mut out = vec![root.join(SHARED)];
    if workspace != root {
        out.push(workspace.join(SHARED));
    }
    out.push(root.join(LOCAL));
    if workspace != root {
        out.push(workspace.join(LOCAL));
    }
    out
}

/// read what a workspace needs.
pub fn load(workspace: &Path, root: &Path) -> WorkspaceScripts {
    let mut merged = toml::Table::new();
    let mut from = Vec::new();
    for path in sources(workspace, root) {
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        if let Ok(table) = text.parse::<toml::Table>() {
            merge(&mut merged, table);
            from.push(path.to_string_lossy().into_owned());
        }
    }
    let mut scripts = parse(&merged);
    scripts.from = from;
    if scripts.copy.is_empty() {
        scripts.copy = worktreeinclude(root).unwrap_or_else(|| vec![DEFAULT_COPY.to_owned()]);
    }
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

pub fn parse(t: &toml::Table) -> WorkspaceScripts {
    let text = |v: Option<&Value>| v.and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let scripts = t.get("scripts").and_then(Value::as_table);
    let mut out = WorkspaceScripts {
        setup: text(scripts.and_then(|s| s.get("setup"))),
        archive: text(scripts.and_then(|s| s.get("archive"))),
        concurrent: scripts.and_then(|s| s.get("run_mode")).and_then(Value::as_str) != Some("nonconcurrent"),
        ..WorkspaceScripts::default()
    };
    match scripts.and_then(|s| s.get("run")) {
        // a single command: the one button.
        Some(Value::String(cmd)) if !cmd.trim().is_empty() => out.actions.push(ScriptAction { id: "run".into(), command: cmd.trim().into(), icon: None, default: true }),
        Some(Value::Table(run)) => {
            for (id, a) in run {
                let (command, icon, default) = match a {
                    Value::String(c) => (Some(c.trim().to_owned()), None, false),
                    Value::Table(a) => (text(a.get("command")), text(a.get("icon")), a.get("default").and_then(Value::as_bool).unwrap_or(false)),
                    _ => (None, None, false),
                };
                if let Some(command) = command.filter(|c| !c.is_empty()) {
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
    out.copy = t
        .get("files")
        .and_then(Value::as_table)
        .and_then(|f| f.get("copy"))
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|g| g.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    out
}

/// the settings as a file says them, for writing back.
pub fn to_toml(s: &WorkspaceScripts) -> String {
    let mut scripts = toml::Table::new();
    if let Some(setup) = s.setup.as_deref().filter(|c| !c.trim().is_empty()) {
        scripts.insert("setup".into(), Value::String(setup.trim().into()));
    }
    if let Some(archive) = s.archive.as_deref().filter(|c| !c.trim().is_empty()) {
        scripts.insert("archive".into(), Value::String(archive.trim().into()));
    }
    if !s.concurrent {
        scripts.insert("run_mode".into(), Value::String("nonconcurrent".into()));
    }
    let mut run = toml::Table::new();
    for a in s.actions.iter().filter(|a| !a.id.trim().is_empty() && !a.command.trim().is_empty()) {
        let mut t = toml::Table::new();
        t.insert("command".into(), Value::String(a.command.trim().into()));
        if let Some(icon) = &a.icon {
            t.insert("icon".into(), Value::String(icon.clone()));
        }
        if a.default {
            t.insert("default".into(), Value::Boolean(true));
        }
        run.insert(a.id.trim().into(), Value::Table(t));
    }
    if !run.is_empty() {
        scripts.insert("run".into(), Value::Table(run));
    }
    let mut file = toml::Table::new();
    if !scripts.is_empty() {
        file.insert("scripts".into(), Value::Table(scripts));
    }
    let copy: Vec<Value> = s.copy.iter().map(|g| g.trim()).filter(|g| !g.is_empty()).map(|g| Value::String(g.into())).collect();
    // the default needn't be written down.
    if !copy.is_empty() && !(copy.len() == 1 && copy[0].as_str() == Some(DEFAULT_COPY)) {
        let mut files = toml::Table::new();
        files.insert("copy".into(), Value::Array(copy));
        file.insert("files".into(), Value::Table(files));
    }
    toml::to_string_pretty(&file).unwrap_or_default()
}

/// write a repository's scripts: this machine's file, or the shared one.
pub fn save(s: &WorkspaceScripts, workspace: &Path, root: &Path, local: bool) -> std::io::Result<PathBuf> {
    let path = if local { root.join(LOCAL) } else { workspace.join(SHARED) };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, to_toml(s))?;
    Ok(path)
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

/// run a workspace's archive script before it's removed: shut down what it
/// started, tidy what it left. it has a minute; the workspace goes either way.
pub async fn archive(command: &str, workspace: &Path, env: &[(String, String)]) {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/zsh".into());
    let run = tokio::process::Command::new(shell)
        .arg("-lc")
        .arg(command)
        .current_dir(workspace)
        .envs(env.iter().map(|(k, v)| (k, v)))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .status();
    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), run).await;
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
/// terminals, its scripts.
pub fn env(workspace: &str, root: &str, port: u16) -> Vec<(String, String)> {
    let name = Path::new(workspace).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    vec![
        ("TESSEL_PORT".into(), port.to_string()),
        ("TESSEL_WORKSPACE_NAME".into(), name),
        ("TESSEL_WORKSPACE_PATH".into(), workspace.to_owned()),
        ("TESSEL_ROOT_PATH".into(), root.to_owned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(t: &str) -> toml::Table {
        t.parse().unwrap()
    }

    #[test]
    fn the_settings_say_setup_buttons_files_and_how_scripts_share() {
        let s = parse(&table(
            r#"
            [scripts]
            setup = "pnpm install"
            archive = "docker compose down"
            run_mode = "nonconcurrent"
            [scripts.run.dev]
            command = "pnpm dev --port $TESSEL_PORT"
            [scripts.run.test]
            command = "pnpm test"
            icon = "test-tube"
            default = true
            [files]
            copy = [".env*", "config/local.json"]
            "#,
        ));
        assert_eq!((s.setup.as_deref(), s.archive.as_deref(), s.concurrent), (Some("pnpm install"), Some("docker compose down"), false));
        assert_eq!(s.actions.iter().map(|a| (a.id.as_str(), a.default)).collect::<Vec<_>>(), [("dev", false), ("test", true)]);
        assert_eq!(s.copy, [".env*", "config/local.json"]);
        let one = parse(&table("[scripts]\nrun = \"npm start\""));
        assert_eq!(one.actions.iter().map(|a| (a.id.as_str(), a.default)).collect::<Vec<_>>(), [("run", true)], "a lone command is the button");
        assert!(one.concurrent, "workspaces may run theirs side by side unless told otherwise");
        // what's written reads back the same.
        assert_eq!(parse(&table(&to_toml(&s))).actions, s.actions);
        assert_eq!(parse(&table(&to_toml(&s))).copy, s.copy);
    }

    #[test]
    fn this_machines_file_wins_and_a_branch_can_change_the_shared_one() {
        let root = std::env::temp_dir().join(format!("tessel-setup-{}", uuid::Uuid::new_v4()));
        let ws = root.with_extension("ws");
        std::fs::create_dir_all(root.join(".tessel")).unwrap();
        std::fs::create_dir_all(ws.join(".tessel")).unwrap();
        std::fs::write(root.join(SHARED), "[scripts]\nsetup = \"npm ci\"\n[scripts.run.web]\ncommand = \"npm run dev\"\n").unwrap();
        let s = load(&ws, &root);
        assert_eq!((s.setup.as_deref(), s.copy.clone()), (Some("npm ci"), vec![DEFAULT_COPY.to_owned()]));
        std::fs::write(ws.join(SHARED), "[scripts]\nsetup = \"pnpm install\"\n").unwrap();
        let s = load(&ws, &root);
        assert_eq!(s.setup.as_deref(), Some("pnpm install"), "the workspace's own copy is newer");
        assert_eq!(s.actions.len(), 1, "and keeps what it doesn't say");
        save(&WorkspaceScripts { setup: Some("make".into()), ..WorkspaceScripts::default() }, &ws, &root, true).unwrap();
        assert_eq!(load(&ws, &root).setup.as_deref(), Some("make"), "this machine's file beats both");
        assert_eq!(load(&ws, &root).from.last().map(String::as_str), Some(root.join(LOCAL).to_string_lossy().as_ref()));
        std::fs::write(root.join(".worktreeinclude"), "# secrets\n.env.local\n").unwrap();
        assert_eq!(load(&ws, &root).copy, [".env.local"]);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&ws);
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
        assert!(env.contains(&("TESSEL_PORT".into(), "41010".into())) && env.contains(&("TESSEL_WORKSPACE_NAME".into(), "b".into())));
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

    #[tokio::test]
    async fn the_archive_script_runs_in_the_workspace_with_its_env() {
        let ws = std::env::temp_dir().join(format!("tessel-archive-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&ws).unwrap();
        archive("echo bye $TESSEL_PORT > out.txt", &ws, &[("TESSEL_PORT".into(), "41030".into())]).await;
        assert_eq!(std::fs::read_to_string(ws.join("out.txt")).unwrap().trim(), "bye 41030");
        let _ = std::fs::remove_dir_all(&ws);
    }
}
