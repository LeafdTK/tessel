use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Sandbox {
    pub sealed: bool,
    pub allow: Vec<String>,
}

pub fn config(workspace: &Path, root: &Path) -> Sandbox {
    let (t, _) = crate::setup::merged(workspace, root);
    let s = t.get("sandbox").and_then(toml::Value::as_table);
    Sandbox {
        sealed: s.and_then(|s| s.get("sealed")).and_then(toml::Value::as_bool).unwrap_or(false),
        allow: s.and_then(|s| s.get("allow")).and_then(toml::Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
    }
}

fn quoted(p: &Path) -> String {
    format!("\"{}\"", p.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\""))
}

fn real(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

pub fn profile(work: &Path, repo: &Path, home: &Path, tessel_home: &Path) -> String {
    let writable: Vec<PathBuf> = [
        real(work),
        real(&repo.join(".git")),
        home.join(".claude"),
        home.join(".cache"),
        home.join(".npm"),
        home.join("Library/Caches"),
        home.join(".cargo/registry"),
        home.join(".cargo/git"),
        PathBuf::from("/private/tmp"),
        PathBuf::from("/private/var/folders"),
        PathBuf::from("/dev"),
    ]
    .into_iter()
    .collect();
    let hidden: Vec<PathBuf> = [
        home.join(".ssh"),
        home.join(".aws"),
        home.join(".gnupg"),
        home.join(".config/gh"),
        home.join(".netrc"),
        tessel_home.join("egress"),
        tessel_home.join("holdouts"),
        tessel_home.join("keys.toml"),
    ]
    .into_iter()
    .collect();
    let mut p = String::from("(version 1)\n(allow default)\n(deny network*)\n(allow network* (remote ip \"localhost:*\"))\n(allow network-outbound (remote unix-socket))\n(allow network-bind (local ip \"*:*\"))\n(allow network-inbound (local ip \"*:*\"))\n(deny file-write*)\n(allow file-write*\n");
    for w in &writable {
        p.push_str(&format!("  (subpath {})\n", quoted(w)));
    }
    p.push_str(&format!("  (regex #\"^{}/\\.claude\\.json\"))\n", regex::escape(&home.to_string_lossy())));
    p.push_str("(deny file-read* file-write*\n");
    for h in &hidden {
        p.push_str(&format!("  (subpath {})\n", quoted(h)));
    }
    p.push_str(")\n");
    p
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(profile: &str, script: &str, env: &[(&str, String)]) -> (bool, String) {
        let mut c = Command::new("/usr/bin/sandbox-exec");
        c.arg("-p").arg(profile).arg("/bin/sh").arg("-c").arg(script);
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output().unwrap();
        (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
    }

    #[test]
    fn a_sealed_agent_writes_only_its_workspace_and_reaches_only_localhost() {
        let base = tempfile::Builder::new().prefix("seal").tempdir_in(std::env::var("HOME").unwrap() + "/.cache").unwrap_or_else(|_| tempfile::tempdir().unwrap());
        let (work, repo, home, th) = (base.path().join("work"), base.path().join("repo"), base.path().join("home"), base.path().join("home/.tessel"));
        for d in [&work, &repo.join(".git"), &home.join(".ssh"), &th.join("egress"), &base.path().join("elsewhere")] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(home.join(".ssh/id"), "secret").unwrap();
        let profile = profile(&work, &repo, &home, &th);
        let w = work.to_string_lossy().into_owned();
        assert!(run(&profile, &format!("echo hi > '{w}/a.txt'"), &[]).0, "its own workspace");
        assert!(run(&profile, &format!("echo hi > '{}/objects'", repo.join(".git").display()), &[]).0, "its repository's git dir, to commit");
        let (wrote, _) = run(&profile, &format!("echo hi > '{}/b.txt'", base.path().join("elsewhere").display()), &[]);
        assert!(!wrote, "nowhere else");
        let (read, out) = run(&profile, &format!("cat '{}'", home.join(".ssh/id").display()), &[]);
        assert!(!read && !out.contains("secret"), "keys stay unreadable");
        let (direct, _) = run(&profile, "/usr/bin/curl -s -m 5 -o /dev/null https://example.com", &[]);
        assert!(!direct, "no network but localhost");
    }
}
