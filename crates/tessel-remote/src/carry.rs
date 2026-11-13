//! what a person brings to a new machine: their Claude (its sign-in, their
//! CLAUDE.md, settings, skills, commands, agents and MCP servers), who they
//! are to git, and their GitHub sign-in. each is theirs to bring or not.
//!
//! the mac gathers what was picked into a folder (`gather`), it travels as a
//! tarball like tessel itself does, and the host puts each thing in its place
//! (`bring`, run there by `tesselctl remote bring`). the same on any host.
//!
//! secrets (the claude token, the github token) go in files only their owner
//! reads under `~/.tessel/remote`, and the daemon hands them to what it runs
//! (`CLAUDE_CODE_OAUTH_TOKEN`, `GH_TOKEN`). they're never written into
//! anyone's shell config.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// what's here to bring, for the setup flow to offer.
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    /// CLAUDE.md's size.
    pub claude_md: Option<u64>,
    pub settings: bool,
    pub skills: usize,
    pub commands: usize,
    pub agents: usize,
    /// MCP servers set up for every project (user scope).
    pub mcp: usize,
    /// name and email, as git knows them.
    pub git: Option<(String, String)>,
    /// signed in to the GitHub CLI.
    pub gh: bool,
    /// Claude Code is here, so a token for the host can be made here.
    pub claude: bool,
}

/// what was picked to bring.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Carry {
    #[serde(default)]
    pub claude_md: bool,
    #[serde(default)]
    pub settings: bool,
    #[serde(default)]
    pub skills: bool,
    #[serde(default)]
    pub commands: bool,
    #[serde(default)]
    pub agents: bool,
    #[serde(default)]
    pub mcp: bool,
    #[serde(default)]
    pub git: bool,
    #[serde(default)]
    pub gh: bool,
    /// a long-lived token for Claude Code there (`claude setup-token`), made in the setup flow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_token: Option<String>,
}

impl Carry {
    /// what's offered on, for what's there: everything but MCP servers (they
    /// often run programs only this mac has) and the GitHub sign-in (it lets
    /// agents push, which is for the person to say).
    pub fn suggested(inv: &Inventory) -> Self {
        Carry {
            claude_md: inv.claude_md.is_some(),
            settings: inv.settings,
            skills: inv.skills > 0,
            commands: inv.commands > 0,
            agents: inv.agents > 0,
            mcp: false,
            git: inv.git.is_some(),
            gh: false,
            claude_token: None,
        }
    }

    pub fn is_empty(&self) -> bool {
        *self == Carry::default()
    }
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}

fn count(dir: &Path) -> usize {
    std::fs::read_dir(dir).map(|d| d.filter_map(Result::ok).filter(|e| !e.file_name().to_string_lossy().starts_with('.')).count()).unwrap_or(0)
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new(cmd).args(args).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned()).filter(|s| !s.is_empty())
}

fn user_mcp() -> serde_json::Map<String, Value> {
    std::fs::read(home().join(".claude.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v.get("mcpServers").and_then(Value::as_object).cloned()).unwrap_or_default()
}

/// what's on this mac to bring.
pub fn inventory() -> Inventory {
    let claude = home().join(".claude");
    Inventory {
        claude_md: std::fs::metadata(claude.join("CLAUDE.md")).ok().map(|m| m.len()),
        settings: claude.join("settings.json").is_file(),
        skills: count(&claude.join("skills")),
        commands: count(&claude.join("commands")),
        agents: count(&claude.join("agents")),
        mcp: user_mcp().len(),
        git: run("git", &["config", "--global", "user.name"]).zip(run("git", &["config", "--global", "user.email"])),
        gh: run("gh", &["auth", "token"]).is_some(),
        claude: run("claude", &["--version"]).is_some(),
    }
}

/// what's picked, gathered into `into`: `manifest.json`, and `claude/` laid
/// out as `~/.claude` is.
pub fn gather(c: &Carry, into: &Path) -> anyhow::Result<()> {
    let from = home().join(".claude");
    let to = into.join("claude");
    std::fs::create_dir_all(&to)?;
    if c.claude_md && from.join("CLAUDE.md").is_file() {
        std::fs::copy(from.join("CLAUDE.md"), to.join("CLAUDE.md"))?;
    }
    if c.settings
        && let Ok(b) = std::fs::read(from.join("settings.json"))
    {
        let mut v: Value = serde_json::from_slice(&b).context("reading ~/.claude/settings.json")?;
        // what runs this mac's own programs stays here: there, they aren't.
        if let Some(o) = v.as_object_mut() {
            o.remove("hooks");
            o.remove("statusLine");
            o.remove("apiKeyHelper");
        }
        std::fs::write(to.join("settings.json"), serde_json::to_vec_pretty(&v)?)?;
    }
    for (on, dir) in [(c.skills, "skills"), (c.commands, "commands"), (c.agents, "agents")] {
        if on && from.join(dir).is_dir() {
            copy_dir(&from.join(dir), &to.join(dir))?;
        }
    }
    let mut manifest = serde_json::json!({});
    if c.mcp {
        manifest["mcpServers"] = Value::Object(user_mcp());
    }
    if c.git
        && let (Some(name), Some(email)) = (run("git", &["config", "--global", "user.name"]), run("git", &["config", "--global", "user.email"]))
    {
        manifest["git"] = serde_json::json!({"name": name, "email": email});
    }
    if c.gh {
        manifest["gh"] = Value::String(run("gh", &["auth", "token"]).context("the GitHub CLI isn't signed in here (`gh auth login`)")?);
    }
    if let Some(t) = &c.claude_token {
        manifest["claude"] = Value::String(t.clone());
    }
    write_private(&into.join("manifest.json"), &serde_json::to_vec(&manifest)?)?;
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.filter_map(Result::ok) {
        let (src, dst) = (e.path(), to.join(e.file_name()));
        let kind = e.file_type()?;
        if kind.is_dir() {
            copy_dir(&src, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst)?;
        } else if kind.is_symlink()
            && let Ok(target) = std::fs::canonicalize(&src)
        {
            // a link to somewhere on this mac: what it points at goes, not the link.
            if target.is_dir() {
                copy_dir(&target, &dst)?;
            } else if target.is_file() {
                std::fs::copy(&target, &dst)?;
            }
        }
    }
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(path)?;
    f.write_all(bytes)?;
    Ok(())
}

/// what was gathered, as a gzipped tarball (what travels).
pub fn pack(dir: &Path) -> anyhow::Result<Vec<u8>> {
    let out = std::process::Command::new("tar").env("COPYFILE_DISABLE", "1").arg("-czf").arg("-").arg("-C").arg(dir).arg(".").output()?;
    anyhow::ensure!(out.status.success(), "packing: {}", String::from_utf8_lossy(&out.stderr));
    Ok(out.stdout)
}

/// where a host keeps the claude token the daemon hands to agents.
pub fn claude_token_file(home: &Path) -> PathBuf {
    crate::identity::dir(home).join("claude-token")
}

/// where a host keeps the GitHub token the daemon hands to what it runs.
pub fn gh_token_file(home: &Path) -> PathBuf {
    crate::identity::dir(home).join("gh-token")
}

/// on the host: put what was brought (gathered into `from`) in its place.
/// what was there and wasn't brought stays. says what it did.
pub fn bring(from: &Path, tessel_home: &Path) -> anyhow::Result<Vec<String>> {
    let mut did = Vec::new();
    let me = home();
    let claude = me.join(".claude");
    std::fs::create_dir_all(&claude)?;
    let got = from.join("claude");
    if got.join("CLAUDE.md").is_file() {
        std::fs::copy(got.join("CLAUDE.md"), claude.join("CLAUDE.md"))?;
        did.push("CLAUDE.md".into());
    }
    if let Ok(b) = std::fs::read(got.join("settings.json")) {
        let theirs: Value = serde_json::from_slice(&b)?;
        let mut ours: Value = std::fs::read(claude.join("settings.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| serde_json::json!({}));
        // the mac's settings win; anything only this machine had stays.
        if let (Some(o), Some(t)) = (ours.as_object_mut(), theirs.as_object()) {
            for (k, v) in t {
                o.insert(k.clone(), v.clone());
            }
        }
        std::fs::write(claude.join("settings.json"), serde_json::to_vec_pretty(&ours)?)?;
        did.push("settings".into());
    }
    for dir in ["skills", "commands", "agents"] {
        if got.join(dir).is_dir() {
            copy_dir(&got.join(dir), &claude.join(dir))?;
            did.push(dir.into());
        }
    }
    let manifest: Value = std::fs::read(from.join("manifest.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    if let Some(servers) = manifest.get("mcpServers").and_then(Value::as_object) {
        let path = me.join(".claude.json");
        let mut cfg: Value = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_else(|| serde_json::json!({}));
        let entry = cfg.as_object_mut().context("~/.claude.json isn't an object")?.entry("mcpServers").or_insert_with(|| serde_json::json!({}));
        if let Some(o) = entry.as_object_mut() {
            for (k, v) in servers {
                o.insert(k.clone(), v.clone());
            }
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&cfg)?)?;
        did.push(format!("{} MCP servers", servers.len()));
    }
    if let Some(g) = manifest.get("git") {
        for (k, key) in [("name", "user.name"), ("email", "user.email")] {
            if let Some(v) = g.get(k).and_then(Value::as_str) {
                let _ = std::process::Command::new("git").args(["config", "--global", key, v]).status();
            }
        }
        did.push("git identity".into());
    }
    if let Some(t) = manifest.get("gh").and_then(Value::as_str) {
        write_private(&gh_token_file(tessel_home), t.as_bytes())?;
        // git itself, for pushes over https: the same token, from the daemon's environment.
        let _ = std::process::Command::new("git").args(["config", "--global", "credential.https://github.com.helper", "!f() { test -n \"$GH_TOKEN\" && echo username=x-access-token && echo \"password=$GH_TOKEN\"; }; f"]).status();
        did.push("GitHub sign-in".into());
    }
    if let Some(t) = manifest.get("claude").and_then(Value::as_str) {
        write_private(&claude_token_file(tessel_home), t.as_bytes())?;
        did.push("Claude sign-in".into());
    }
    let _ = std::fs::remove_dir_all(from);
    Ok(did)
}

/// the secrets brought here, as the daemon's environment for what it runs.
/// (what's set already is left as it is.)
pub fn secrets_env(tessel_home: &Path) -> Vec<(&'static str, String)> {
    let mut env = Vec::new();
    for (name, file) in [("CLAUDE_CODE_OAUTH_TOKEN", claude_token_file(tessel_home)), ("GH_TOKEN", gh_token_file(tessel_home))] {
        if std::env::var_os(name).is_none()
            && let Ok(t) = std::fs::read_to_string(file)
            && !t.trim().is_empty()
        {
            env.push((name, t.trim().to_owned()));
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_stay_home_and_settings_merge_with_what_was_there() {
        let mac = tempfile::tempdir().unwrap();
        let host = tempfile::tempdir().unwrap();
        // a fake home for each side.
        std::fs::create_dir_all(mac.path().join(".claude/skills/one")).unwrap();
        std::fs::write(mac.path().join(".claude/CLAUDE.md"), "be good").unwrap();
        std::fs::write(mac.path().join(".claude/skills/one/SKILL.md"), "x").unwrap();
        std::fs::write(mac.path().join(".claude/settings.json"), r#"{"model":"opus","hooks":{"Stop":[]},"statusLine":{"command":"/Users/x/s.sh"}}"#).unwrap();
        let staged = mac.path().join("staged");
        unsafe { std::env::set_var("HOME", mac.path()) };
        let c = Carry { claude_md: true, settings: true, skills: true, claude_token: Some("sk-ant-oat01-test".into()), ..Carry::default() };
        gather(&c, &staged).unwrap();
        let settings: Value = serde_json::from_slice(&std::fs::read(staged.join("claude/settings.json")).unwrap()).unwrap();
        assert!(settings.get("hooks").is_none() && settings.get("statusLine").is_none(), "this mac's programs stay here");
        std::fs::create_dir_all(host.path().join(".claude")).unwrap();
        std::fs::write(host.path().join(".claude/settings.json"), r#"{"theme":"dark","model":"haiku"}"#).unwrap();
        unsafe { std::env::set_var("HOME", host.path()) };
        let th = host.path().join(".tessel");
        let did = bring(&staged, &th).unwrap();
        assert!(did.contains(&"Claude sign-in".to_owned()), "{did:?}");
        let merged: Value = serde_json::from_slice(&std::fs::read(host.path().join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!((merged["model"].as_str(), merged["theme"].as_str()), (Some("opus"), Some("dark")));
        assert!(host.path().join(".claude/skills/one/SKILL.md").is_file());
        assert_eq!(std::fs::read_to_string(claude_token_file(&th)).unwrap(), "sk-ant-oat01-test");
        let mode = std::fs::metadata(claude_token_file(&th)).unwrap().permissions();
        assert_eq!(std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777, 0o600);
    }
}
