use std::path::{Path, PathBuf};
use tessel_proto::proof::Verified;
use tessel_remote::vercel::{Account, Sandbox};

pub struct Lab {
    pub name: String,
    account: Account,
    session: String,
    dir: String,
    path: Option<String>,
}

pub const SHOTS: &str = ".tessel-shots";

async fn bundle(ws: &Path) -> Result<Vec<u8>, String> {
    let files = crate::ship::git(ws, &["ls-files", "-co", "--exclude-standard"]).await?;
    let list = std::env::temp_dir().join(format!("tessel-lab-{}.txt", uuid::Uuid::new_v4().simple()));
    std::fs::write(&list, files.lines().filter(|f| ws.join(f).is_file()).collect::<Vec<_>>().join("\n")).map_err(|e| e.to_string())?;
    let out = tokio::process::Command::new("tar").arg("czf").arg("-").arg("-C").arg(ws).arg("-T").arg(&list).env("COPYFILE_DISABLE", "1").output().await.map_err(|e| e.to_string());
    let _ = std::fs::remove_file(&list);
    let out = out?;
    if !out.status.success() {
        return Err(format!("packing the workspace: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

impl Lab {
    pub async fn open(home: &Path, name: &str, ws: &Path, setup: &[String]) -> Result<Lab, String> {
        let hosts = tessel_remote::hosts::Hosts::load(home);
        let host = hosts.find(name).ok_or_else(|| format!("there's no host called {name} (tesselctl host add)"))?;
        let sb: Sandbox = host.vercel.clone().ok_or_else(|| format!("{name} isn't a vercel sandbox"))?;
        let account = Account::load(home, &sb).map_err(|e| format!("{e:#}"))?;
        let session = match account.wake(&sb).await.map_err(|e| format!("{e:#}"))? {
            Some(s) => s,
            None => account.create(&sb).await.map_err(|e| format!("{e:#}"))?,
        };
        let dir = format!("/tmp/proof-{}", uuid::Uuid::new_v4().simple());
        let lab = Lab { name: host.name.clone(), account, session: session.id, dir, path: sb.path.clone() };
        let tgz = bundle(ws).await?;
        lab.account.upload(&lab.session, &lab.dir, tgz, &mut |_| {}).await.map_err(|e| format!("sending the workspace: {e:#}"))?;
        for command in setup {
            let v = lab.run(command, &[]).await;
            if !v.ok {
                lab.close().await;
                return Err(format!("setting it up, {command} failed:\n{}", v.output));
            }
        }
        Ok(lab)
    }

    pub async fn run(&self, command: &str, env: &[(&str, &str)]) -> Verified {
        let start = std::time::Instant::now();
        let mut all: Vec<(&str, &str)> = vec![("PYTHONDONTWRITEBYTECODE", "1")];
        let path;
        if let Some(p) = &self.path {
            path = p.clone();
            all.push(("PATH", &path));
        }
        all.extend_from_slice(env);
        let script = format!("cd '{}' && {command}", self.dir);
        let (ok, output) = match self.account.run(&self.session, "sh", &["-c", &script], &all).await {
            Ok((code, out)) => (code == 0, crate::proof::trimmed(&out)),
            Err(e) => (false, format!("couldn't run it in the lab: {e:#}")),
        };
        Verified { command: command.to_owned(), ok, output, ms: start.elapsed().as_millis() as u64, rule: None }
    }

    pub fn shots_dir(&self) -> String {
        format!("{}/{SHOTS}", self.dir)
    }

    pub async fn fetch_shots(&self, into: &Path) -> Vec<PathBuf> {
        let script = format!("cd '{}' 2>/dev/null && for f in *.png; do [ -f \"$f\" ] && echo \"@@ $f\" && base64 < \"$f\" | tr -d '\\n' && echo; done", self.shots_dir());
        let Ok((_, out)) = self.account.run(&self.session, "sh", &["-c", &script], &[]).await else { return Vec::new() };
        decode_shots(&out, into)
    }

    pub async fn close(&self) {
        let _ = self.account.run(&self.session, "rm", &["-rf", &self.dir], &[]).await;
    }
}

pub fn decode_shots(out: &str, into: &Path) -> Vec<PathBuf> {
    use base64::Engine as _;
    let _ = std::fs::create_dir_all(into);
    let mut saved = Vec::new();
    let mut lines = out.lines();
    while let Some(l) = lines.next() {
        let Some(name) = l.strip_prefix("@@ ") else { continue };
        let name = Path::new(name.trim()).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let Some(data) = lines.next() else { break };
        if name.is_empty() || !name.ends_with(".png") {
            continue;
        }
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data.trim()) {
            let p = into.join(&name);
            if std::fs::write(&p, bytes).is_ok() {
                saved.push(p);
            }
        }
    }
    saved.sort();
    saved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screenshots_come_back_from_the_lab_as_files() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let png = [0x89u8, b'P', b'N', b'G', 1, 2, 3];
        let out = format!("noise\n@@ board.png\n{}\n@@ ../escape.png\n{}\n@@ notes.txt\naGk=\n", base64::engine::general_purpose::STANDARD.encode(png), base64::engine::general_purpose::STANDARD.encode(png));
        let got = decode_shots(&out, dir.path());
        assert_eq!(got.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>(), vec!["board.png", "escape.png"], "names stay inside the folder, and only pictures");
        assert_eq!(std::fs::read(&got[0]).unwrap(), png);
    }
}
