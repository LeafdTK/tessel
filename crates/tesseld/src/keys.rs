use std::path::{Path, PathBuf};
use tessel_proto::keys::KeyInfo;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct File {
    #[serde(default)]
    key: Vec<KeyInfo>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join("keys.toml")
}

pub fn load(home: &Path) -> Vec<KeyInfo> {
    let Ok(text) = std::fs::read_to_string(path(home)) else { return Vec::new() };
    toml::from_str::<File>(&text).map(|f| f.key).unwrap_or_default()
}

pub fn save(home: &Path, keys: &[KeyInfo]) -> std::io::Result<()> {
    let keys: Vec<KeyInfo> = keys.iter().map(|k| KeyInfo { stored: false, uses: 0, ..k.clone() }).collect();
    let text = toml::to_string(&File { key: keys }).map_err(std::io::Error::other)?;
    std::fs::write(path(home), text)
}

pub fn check(k: &KeyInfo) -> Result<(), String> {
    if k.name.is_empty() || !k.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err("a key's name is letters, digits, - and _".into());
    }
    if k.hosts.is_empty() {
        return Err("say which hosts it's for".into());
    }
    if let Some(bad) = k.hosts.iter().find(|h| h.is_empty() || h.contains('/') || h.contains(':') || (h.contains('*') && !h.starts_with("*."))) {
        return Err(format!("\"{bad}\" isn't a host: write api.github.com, or *.example.com"));
    }
    if !k.format.contains("{key}") {
        return Err("the header's format needs {key} where the secret goes".into());
    }
    if let Some(env) = &k.env
        && (env.is_empty() || !env.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
    {
        return Err(format!("{env} isn't an environment variable name"));
    }
    Ok(())
}

pub fn host_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    match pattern.strip_prefix("*.") {
        Some(base) => host.ends_with(&format!(".{base}")),
        None => host == pattern.to_ascii_lowercase(),
    }
}

pub fn for_host<'a>(keys: &'a [KeyInfo], host: &str) -> Option<&'a KeyInfo> {
    keys.iter().find(|k| k.hosts.iter().any(|h| host_matches(h, host)))
}

pub fn allows(k: &KeyInfo, method: &str, path: &str) -> bool {
    let method_ok = k.methods.is_empty() || k.methods.iter().any(|m| m.eq_ignore_ascii_case(method));
    let path = path.split('?').next().unwrap_or(path);
    let path_ok = k.paths.is_empty()
        || k.paths.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == p,
        });
    method_ok && path_ok
}

#[cfg(target_os = "macos")]
pub fn secret(home: &Path, name: &str) -> Option<String> {
    let _ = home;
    let out = std::process::Command::new("security").args(["find-generic-password", "-s", "tessel", "-a", &format!("key:{name}"), "-w"]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim_end_matches('\n').to_owned()).filter(|s| !s.is_empty())
}

#[cfg(target_os = "macos")]
pub fn store(home: &Path, name: &str, value: &str) -> Result<(), String> {
    let _ = home;
    use std::io::Write as _;
    if value.contains('\n') {
        return Err("a key is one line".into());
    }
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut child = std::process::Command::new("security")
        .arg("-i")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let line = format!("add-generic-password -U -s tessel -a {} -l {} -w {}\n", quote(&format!("key:{name}")), quote(&format!("tessel: {name} key")), quote(value));
    child.stdin.take().ok_or("no stdin")?.write_all(line.as_bytes()).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if out.status.success() && out.stderr.is_empty() { Ok(()) } else { Err(format!("the keychain refused it: {}", String::from_utf8_lossy(&out.stderr).trim())) }
}

#[cfg(target_os = "macos")]
pub fn forget(home: &Path, name: &str) {
    let _ = home;
    let _ = std::process::Command::new("security").args(["delete-generic-password", "-s", "tessel", "-a", &format!("key:{name}")]).output();
}

#[cfg(not(target_os = "macos"))]
fn secret_file(home: &Path, name: &str) -> PathBuf {
    home.join("keys").join(name)
}

#[cfg(not(target_os = "macos"))]
pub fn secret(home: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(secret_file(home, name)).ok().map(|s| s.trim_end_matches('\n').to_owned()).filter(|s| !s.is_empty())
}

#[cfg(not(target_os = "macos"))]
pub fn store(home: &Path, name: &str, value: &str) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt;
    let path = secret_file(home, name);
    std::fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
    let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(true).mode(0o600).open(&path).map_err(|e| e.to_string())?;
    std::io::Write::write_all(&mut f, value.as_bytes()).map_err(|e| e.to_string())
}

#[cfg(not(target_os = "macos"))]
pub fn forget(home: &Path, name: &str) {
    let _ = std::fs::remove_file(secret_file(home, name));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> KeyInfo {
        KeyInfo {
            name: "github".into(),
            hosts: vec!["api.github.com".into(), "*.githubusercontent.com".into()],
            header: "authorization".into(),
            format: "Bearer {key}".into(),
            env: Some("GH_TOKEN".into()),
            methods: vec!["GET".into(), "POST".into()],
            paths: vec!["/repos/acme/web/*".into(), "/user".into()],
            stored: false,
            uses: 0,
        }
    }

    #[test]
    fn a_key_is_used_only_for_its_hosts_methods_and_paths() {
        let keys = [github()];
        assert!(for_host(&keys, "api.github.com").is_some());
        assert!(for_host(&keys, "raw.githubusercontent.com").is_some());
        assert!(for_host(&keys, "githubusercontent.com").is_none(), "a wildcard is for what's under it");
        assert!(for_host(&keys, "evil-api.github.com.example").is_none());
        let k = &keys[0];
        assert!(allows(k, "get", "/repos/acme/web/pulls?state=open"));
        assert!(allows(k, "GET", "/user"));
        assert!(!allows(k, "GET", "/user/keys"), "an exact path is exact");
        assert!(!allows(k, "DELETE", "/repos/acme/web"), "not a method it was given");
        assert!(!allows(k, "GET", "/repos/acme/other/pulls"));
    }

    #[test]
    fn keys_round_trip_without_their_secrets_and_bad_ones_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        save(dir.path(), &[KeyInfo { stored: true, uses: 9, ..github() }]).unwrap();
        let text = std::fs::read_to_string(path(dir.path())).unwrap();
        assert!(!text.contains("stored = true") && text.contains("api.github.com"));
        assert_eq!(load(dir.path()), vec![github()]);
        assert!(check(&github()).is_ok());
        assert!(check(&KeyInfo { hosts: vec!["https://api.github.com".into()], ..github() }).is_err());
        assert!(check(&KeyInfo { format: "Bearer".into(), ..github() }).is_err());
        assert!(check(&KeyInfo { name: "has space".into(), ..github() }).is_err());
    }
}
