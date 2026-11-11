//! the "currently doing" line: one short human sentence for a tool call.
//!
//! here rather than in the daemon so every client words a tool call the same way.

use serde_json::Value;

const MAX: usize = 80;

/// a short description of a tool call, from whatever fields it has.
/// works on field names, not a fixed tool list, so a tool we've never seen
/// still gets something better than nothing.
pub fn summarize_tool(tool: &str, input: &Value) -> String {
    let s = |k: &str| input.get(k).and_then(Value::as_str).filter(|v| !v.trim().is_empty());
    let line = if let Some(path) = s("file_path").or_else(|| s("notebook_path")) {
        let verb = match tool {
            "Read" => "reading",
            "Write" => "writing",
            _ => "editing",
        };
        format!("{verb} {}", basename(path))
    } else if let Some(cmd) = s("command") {
        // agents usually describe their own commands better than the command does.
        match s("description") {
            Some(d) => lower_first(d),
            None => format!("running {}", first_line(cmd)),
        }
    } else if let Some(pattern) = s("pattern") {
        format!("searching for {pattern}")
    } else if let Some(url) = s("url") {
        format!("fetching {url}")
    } else if tool == "ToolSearch" {
        // the agent fetching one of its own tools, not looking something up for you.
        "loading a tool".to_owned()
    } else if let Some(q) = s("query") {
        format!("searching the web for {q}")
    } else if let Some(d) = s("description") {
        match tool {
            "Agent" | "Task" => format!("subagent: {}", lower_first(d)),
            _ => lower_first(d),
        }
    } else {
        format!("using {tool}")
    };
    truncate(&line)
}

pub fn truncate(s: &str) -> String {
    let s = first_line(s);
    if s.chars().count() <= MAX {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(MAX - 1).collect();
    out.push('…');
    out
}

fn first_line(s: &str) -> &str {
    s.trim().lines().next().unwrap_or("").trim()
}

fn basename(path: &str) -> &str {
    path.rsplit('/').find(|p| !p.is_empty()).unwrap_or(path)
}

fn lower_first(s: &str) -> String {
    let s = first_line(s);
    let mut chars = s.chars();
    match chars.next() {
        // keep acronyms and proper nouns: only lower "Run tests", not "URL check".
        Some(c) if c.is_uppercase() && chars.clone().next().is_some_and(|n| n.is_lowercase()) => {
            c.to_lowercase().collect::<String>() + chars.as_str()
        }
        _ => s.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn files() {
        assert_eq!(summarize_tool("Edit", &json!({"file_path": "/a/b/src/main.rs"})), "editing main.rs");
        assert_eq!(summarize_tool("Read", &json!({"file_path": "/a/b/Cargo.toml"})), "reading Cargo.toml");
        assert_eq!(summarize_tool("Write", &json!({"file_path": "notes.md", "content": "x"})), "writing notes.md");
    }

    #[test]
    fn commands_prefer_their_description() {
        assert_eq!(summarize_tool("Bash", &json!({"command": "cargo test -q", "description": "Run the tests"})), "run the tests");
        assert_eq!(summarize_tool("Bash", &json!({"command": "cargo test -q\necho done"})), "running cargo test -q");
    }

    #[test]
    fn search_fetch_subagent_and_unknown() {
        assert_eq!(summarize_tool("Grep", &json!({"pattern": "fn main"})), "searching for fn main");
        assert_eq!(summarize_tool("WebFetch", &json!({"url": "https://example.com"})), "fetching https://example.com");
        assert_eq!(summarize_tool("Agent", &json!({"description": "Audit the parser", "prompt": "..."})), "subagent: audit the parser");
        assert_eq!(summarize_tool("ToolSearch", &json!({"query": "select:ExitPlanMode", "max_results": 1})), "loading a tool", "not a web search");
        assert_eq!(summarize_tool("Mystery", &json!({})), "using Mystery");
        assert_eq!(summarize_tool("Mystery", &Value::Null), "using Mystery");
    }

    #[test]
    fn long_lines_are_cut_on_a_char_boundary() {
        let long = "é".repeat(300);
        let out = summarize_tool("Bash", &json!({"command": long}));
        assert_eq!(out.chars().count(), MAX);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn acronyms_keep_their_case() {
        assert_eq!(summarize_tool("Bash", &json!({"command": "x", "description": "URL check"})), "URL check");
    }
}
