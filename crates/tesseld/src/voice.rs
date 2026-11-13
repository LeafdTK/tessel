//! dictation tidied: a fast model makes what was said into what was meant.
//! groq's when the person has given tessel a key (it answers in a few hundred
//! milliseconds), else haiku through the claude cli (a few seconds). either
//! way there's a deadline, and past it (or on any failure) the answer is what
//! was said with its noises left out, so the person is never left waiting.

use std::sync::Arc;
use std::time::Duration;
use tessel_agent::backend::Backend;
use tessel_proto::voice::{plausible, strip_noises, tidy_prompt};

/// groq's fastest model, for this.
const GROQ_MODEL: &str = "openai/gpt-oss-20b";
const GROQ_URL: &str = "https://api.groq.com/openai/v1/chat/completions";

/// the person's groq key: `GROQ_API_KEY`, else the macOS keychain (service
/// "tessel", account "groq"; `tesselctl secret groq` puts it there).
pub async fn groq_key() -> Option<String> {
    if let Some(k) = std::env::var("GROQ_API_KEY").ok().filter(|k| !k.trim().is_empty()) {
        return Some(k.trim().to_owned());
    }
    let out = tokio::process::Command::new("security").args(["find-generic-password", "-s", "tessel", "-a", "groq", "-w"]).output().await.ok()?;
    let key = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !key.is_empty()).then_some(key)
}

/// ask groq. the key goes to curl on its stdin (its config), never in its arguments.
async fn groq(key: &str, prompt: &str) -> Result<String, String> {
    use tokio::io::AsyncWriteExt;
    let body = serde_json::json!({
        "model": GROQ_MODEL,
        "messages": [{"role": "user", "content": prompt}],
        "temperature": 0,
        "reasoning_effort": "low",
        "max_completion_tokens": 2048,
    });
    let dir = tempfile_dir()?;
    let body_path = dir.join("body.json");
    std::fs::write(&body_path, body.to_string()).map_err(|e| e.to_string())?;
    let config = format!(
        "url = \"{GROQ_URL}\"\nheader = \"Authorization: Bearer {key}\"\nheader = \"Content-Type: application/json\"\ndata-binary = \"@{}\"\nsilent\nshow-error\nmax-time = 8\n",
        body_path.display()
    );
    let mut child = tokio::process::Command::new("curl")
        .args(["--config", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(config.as_bytes()).await.map_err(|e| e.to_string())?;
    }
    let out = child.wait_with_output().await.map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|_| format!("groq: {}", String::from_utf8_lossy(&out.stderr).trim()))?;
    if let Some(e) = v.pointer("/error/message").and_then(serde_json::Value::as_str) {
        return Err(format!("groq: {e}"));
    }
    v.pointer("/choices/0/message/content").and_then(serde_json::Value::as_str).map(str::to_owned).ok_or_else(|| "groq gave no answer".to_owned())
}

/// a private folder for the request's body (it holds what was said).
fn tempfile_dir() -> Result<std::path::PathBuf, String> {
    let dir = std::env::temp_dir().join(format!("tessel-voice-{}-{}", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    Ok(dir)
}

/// what was said, tidied, and what tidied it.
pub async fn tidy(backend: Arc<dyn Backend>, text: String, context: String) -> (String, &'static str) {
    let plain = strip_noises(&text);
    // a word or two has nothing for a model to fix.
    if plain.split_whitespace().count() < 4 {
        return (plain, "filter");
    }
    let prompt = tidy_prompt(&plain, &context);
    let asked = async {
        match groq_key().await {
            Some(key) => groq(&key, &prompt).await.map(|t| (t, "groq")),
            None => backend.one_shot(prompt.clone(), "haiku").await.map(|t| (t, "haiku")).map_err(|e| format!("{e:#}")),
        }
    };
    match tokio::time::timeout(Duration::from_secs(12), asked).await {
        Ok(Ok((t, by))) if plausible(&plain, &t) => (t.trim().to_owned(), by),
        Ok(Ok(_)) => (plain, "filter"),
        Ok(Err(e)) => {
            eprintln!("tesseld: tidying dictation: {e}");
            (plain, "filter")
        }
        Err(_) => (plain, "filter"),
    }
}

/// what the chat works on, for spelling its names: its goal, and its files.
pub async fn context(dir: &std::path::Path, goal: &str) -> String {
    let files = tokio::process::Command::new("git").args(["ls-files"]).current_dir(dir).output().await.ok().filter(|o| o.status.success()).map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
    let mut names: Vec<&str> = files.lines().collect();
    names.truncate(400);
    let mut out = format!("goal: {goal}\nfolder: {}\nfiles:\n{}", dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), names.join("\n"));
    out.truncate(6000);
    out
}
