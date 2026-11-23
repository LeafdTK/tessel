use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tessel_proto::Choice;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

#[derive(Clone, Debug, PartialEq)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

fn stdio_server(v: &Value) -> Option<Server> {
    let kind = v.get("type").and_then(Value::as_str).unwrap_or("stdio");
    if kind != "stdio" {
        return None;
    }
    Some(Server {
        command: v.get("command")?.as_str()?.to_owned(),
        args: v.get("args").and_then(Value::as_array).map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect()).unwrap_or_default(),
        env: v.get("env").and_then(Value::as_object).map(|o| o.iter().filter_map(|(k, x)| x.as_str().map(|s| (k.clone(), s.to_owned()))).collect()).unwrap_or_default(),
    })
}

pub fn server_config(name: &str, user_home: &Path, cwd: Option<&Path>) -> Result<Server, String> {
    let read = |p: &Path| std::fs::read_to_string(p).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if let Some(cwd) = cwd {
        for dir in cwd.ancestors() {
            if let Some(v) = read(&dir.join(".mcp.json")).and_then(|c| c.get("mcpServers")?.get(name).cloned()) {
                return stdio_server(&v).ok_or_else(|| format!("{name} isn't a server tessel can start itself (only local ones are)"));
            }
        }
    }
    let config = read(&user_home.join(".claude.json")).unwrap_or(Value::Null);
    let project = cwd.and_then(|c| config.get("projects")?.get(c.to_str()?)?.get("mcpServers")?.get(name).cloned());
    let found = project.or_else(|| config.get("mcpServers")?.get(name).cloned()).ok_or_else(|| format!("there's no tool server called {name} in Claude's settings"))?;
    stdio_server(&found).ok_or_else(|| format!("{name} isn't a server tessel can start itself (only local ones are)"))
}

pub async fn call(server: &Server, tool: &str, args: Value, timeout: Duration) -> Result<Value, String> {
    let mut child = Command::new(&server.command)
        .args(&server.args)
        .envs(&server.env)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't start {}: {e}", server.command))?;
    let mut stdin = child.stdin.take().ok_or("no input to the server")?;
    let mut lines = BufReader::new(child.stdout.take().ok_or("no output from the server")?).lines();
    let talk = async {
        let send = |v: Value| format!("{v}\n");
        stdin
            .write_all(send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "tessel", "version": env!("CARGO_PKG_VERSION")}}})).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        answer(&mut lines, 1).await?;
        stdin.write_all(send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"})).as_bytes()).await.map_err(|e| e.to_string())?;
        let args = if args.is_null() { json!({}) } else { args };
        stdin.write_all(send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": args}})).as_bytes()).await.map_err(|e| e.to_string())?;
        answer(&mut lines, 2).await
    };
    let result = tokio::time::timeout(timeout, talk).await.map_err(|_| format!("{tool} took longer than {}s", timeout.as_secs()))??;
    let _ = child.kill().await;
    if result.get("isError").and_then(Value::as_bool) == Some(true) {
        return Err(texts(&result).join(" "));
    }
    if let Some(s) = result.get("structuredContent").filter(|v| !v.is_null()) {
        return Ok(match s.get("result") {
            Some(inner) if s.as_object().is_some_and(|o| o.len() == 1) => inner.clone(),
            _ => s.clone(),
        });
    }
    let parts: Vec<Value> = texts(&result).iter().map(|t| serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.clone()))).collect();
    Ok(match parts.len() {
        1 => parts.into_iter().next().unwrap_or(Value::Null),
        _ => Value::Array(parts),
    })
}

async fn answer<R: tokio::io::AsyncBufRead + Unpin>(lines: &mut tokio::io::Lines<R>, id: u64) -> Result<Value, String> {
    while let Some(line) = lines.next_line().await.map_err(|e| e.to_string())? {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg.get("id").and_then(Value::as_u64) != Some(id) {
            continue;
        }
        if let Some(e) = msg.get("error") {
            return Err(e.get("message").and_then(Value::as_str).unwrap_or("the server said no").to_owned());
        }
        return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
    }
    Err("the server stopped before answering".into())
}

fn texts(result: &Value) -> Vec<String> {
    result.get("content").and_then(Value::as_array).map(|c| c.iter().filter_map(|p| p.get("text").and_then(Value::as_str).map(str::to_owned)).collect()).unwrap_or_default()
}

fn rows(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.values().find(|x| x.is_array()).map(rows).unwrap_or_else(|| vec![v.clone()]),
        Value::String(s) => match serde_json::from_str::<Value>(s) {
            Ok(parsed) if !parsed.is_string() => rows(&parsed),
            _ => s.lines().map(str::trim).filter(|l| !l.is_empty()).map(|l| Value::String(l.to_owned())).collect(),
        },
        _ => Vec::new(),
    }
}

pub fn choices(result: &Value, label: Option<&str>, value: Option<&str>) -> Vec<Choice> {
    let field = |row: &Value, name: &str| -> Option<String> {
        let mut at = row;
        for part in name.split('.') {
            at = at.get(part)?;
        }
        Some(crate::workflow::plain(at)).filter(|s| !s.is_empty())
    };
    let first = |row: &Value, names: &[&str]| names.iter().find_map(|n| field(row, n));
    rows(result)
        .iter()
        .filter_map(|row| {
            if let Value::String(s) = row {
                return Some(Choice { value: s.clone(), label: s.clone(), note: None });
            }
            let label = match label {
                Some(t) => crate::workflow::render_with(t, |n| field(row, n)),
                None => first(row, &["name", "title", "label", "course_code"])?,
            };
            let value = match value {
                Some(t) => crate::workflow::render_with(t, |n| field(row, n)),
                None => first(row, &["id", "value", "name", "title"])?,
            };
            (!label.is_empty() && !value.is_empty()).then(|| Choice { value, label, note: None })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tools_rows_become_choices_whatever_shape_they_come_in() {
        let courses = json!([{"id": 42, "name": "Calculus II", "term": "Fall"}, {"id": 7, "name": "Physics"}, {"id": 9}]);
        let c = choices(&courses, None, None);
        assert_eq!(c.iter().map(|c| (c.value.as_str(), c.label.as_str())).collect::<Vec<_>>(), [("42", "Calculus II"), ("7", "Physics")], "a row with no name is left out");
        let c = choices(&json!({"items": courses}), Some("{{name}} · {{term}}"), Some("{{id}}"));
        assert_eq!(c[0].label, "Calculus II · Fall");
        assert_eq!(choices(&json!("[{\"id\":1,\"title\":\"Midterm\"}]"), None, None)[0].label, "Midterm");
        assert_eq!(choices(&json!("Limits\nSeries\n"), None, None).len(), 2);
    }

    #[test]
    fn a_server_is_found_in_claudes_settings_and_a_remote_one_is_said_to_be() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".claude.json"), json!({"mcpServers": {"canvas": {"type": "stdio", "command": "/py", "args": ["server.py"], "env": {"K": "v"}}, "docs": {"type": "http", "url": "https://x"}}}).to_string()).unwrap();
        let s = server_config("canvas", tmp.path(), None).unwrap();
        assert_eq!((s.command.as_str(), s.args.as_slice(), s.env.get("K").map(String::as_str)), ("/py", &["server.py".to_owned()][..], Some("v")));
        assert!(server_config("docs", tmp.path(), None).unwrap_err().contains("only local ones"));
        assert!(server_config("nope", tmp.path(), None).unwrap_err().contains("no tool server called nope"));
    }

    #[tokio::test]
    async fn a_tool_is_called_over_stdio_like_claude_would() {
        let tmp = tempfile::tempdir().unwrap();
        let script = tmp.path().join("server.py");
        std::fs::write(
            &script,
            r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"protocolVersion": "2025-06-18", "capabilities": {}, "serverInfo": {"name": "fake", "version": "1"}}}), flush=True)
    elif m.get("method") == "tools/call":
        rows = [{"id": 1, "name": "Course " + str(m["params"]["arguments"].get("n", 0))}]
        print(json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": {"content": [{"type": "text", "text": json.dumps(rows)}], "structuredContent": {"result": rows}}}), flush=True)
"#,
        )
        .unwrap();
        let server = Server { command: "python3".into(), args: vec![script.to_string_lossy().into_owned()], env: HashMap::new() };
        let got = call(&server, "list_courses", json!({"n": 3}), Duration::from_secs(20)).await.unwrap();
        assert_eq!(choices(&got, None, None), [Choice { value: "1".into(), label: "Course 3".into(), note: None }]);
    }
}

#[cfg(test)]
mod live {
    #[tokio::test]
    #[ignore]
    async fn canvas_courses_for_real() {
        let home = std::path::PathBuf::from(std::env::var("HOME").unwrap());
        let s = super::server_config("canvas", &home, None).unwrap();
        let t = std::time::Instant::now();
        let got = super::call(&s, "list_courses", serde_json::Value::Null, std::time::Duration::from_secs(60)).await.unwrap();
        let c = super::choices(&got, None, None);
        println!("{} courses in {:?}: {:?}", c.len(), t.elapsed(), c.iter().map(|c| &c.label).take(8).collect::<Vec<_>>());
        assert!(!c.is_empty());
    }
}
