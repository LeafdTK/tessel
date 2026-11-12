//! every command the window understands, in one table.
//!
//! "if you can click it, you can script it": the palette, the keys, the cli
//! (`tesselctl ui …`) and agents' tools all run the same entries through
//! `Shell::exec`, so none of them can drift from the others. the table is
//! static: listing or searching it allocates nothing until something is shown.

/// one command, or one preset of a command's arguments.
#[derive(Clone, Copy, Debug)]
pub struct Spec {
    /// what scripts call it: `pane.split`.
    pub id: &'static str,
    /// what people see: "Split right".
    pub title: &'static str,
    /// a lucide icon.
    pub icon: &'static str,
    /// the default shortcut, as keycaps: "⇧ ⌘ D".
    pub keys: Option<&'static str>,
    /// arguments this entry runs with (json), for presets like "Split down".
    pub preset: &'static str,
    /// what it takes, for the reference and `tesselctl ui commands`.
    pub args: &'static str,
    /// shown in the palette. queries and plumbing (`ui.ls`) aren't.
    pub palette: bool,
}

const fn c(id: &'static str, title: &'static str, icon: &'static str, keys: Option<&'static str>, preset: &'static str, args: &'static str) -> Spec {
    Spec { id, title, icon, keys, preset, args, palette: true }
}

const fn q(id: &'static str, title: &'static str, args: &'static str) -> Spec {
    Spec { id, title, icon: "terminal", keys: None, preset: "{}", args, palette: false }
}

pub static COMMANDS: &[Spec] = &[
    // the window
    c("app.palette", "Command palette", "command", Some("⌘ K"), "{}", ""),
    c("app.settings", "Settings", "settings", Some("⌘ ,"), "{}", ""),
    c("app.overview", "All sessions", "layout-grid", Some("⇧ ⌘ \\"), "{}", ""),
    c("app.sidebar", "Show or hide the sidebar", "panel-left", Some("⌥ ⌘ S"), "{}", ""),
    c("app.workspace", "Show or hide the workspace", "panel-right", Some("⌥ ⌘ I"), "{}", ""),
    c("app.focus", "Focus on this session", "focus", Some("⇧ ⌘ F"), "{}", ""),
    // sessions
    c("session.new", "New session", "square-pen", Some("⌘ N"), "{}", "cwd?, prompt?, worktree?: with a prompt it starts at once"),
    c("session.next", "Next session that needs you", "bell", Some("⌘ J"), "{}", ""),
    c("session.stop", "Stop the agent", "square", Some("⌘ ."), "{}", "session?"),
    c("session.allow", "Allow", "check", Some("⌘ ↩"), "{}", "session?"),
    c("composer.attach", "Attach files…", "paperclip", Some("⌘ U"), "{}", "opens the file picker; what you choose goes with the next message"),
    q("session.open", "Open a session", "session: id prefix, @open, or part of its name"),
    q("session.send", "Send a message", "text, session?"),
    q("session.rename", "Rename a session", "name, session?"),
    q("session.delete", "Delete a session", "session"),
    q("session.model", "Change the model", "model, session?"),
    c("model.favorite", "Use favourite model 1", "box", Some("⌃ ⌘ 1"), r#"{"n":1}"#, "n: 1-5, the favourite's place; session?"),
    c("model.favorite", "Use favourite model 2", "box", Some("⌃ ⌘ 2"), r#"{"n":2}"#, ""),
    c("model.favorite", "Use favourite model 3", "box", Some("⌃ ⌘ 3"), r#"{"n":3}"#, ""),
    c("model.favorite", "Use favourite model 4", "box", Some("⌃ ⌘ 4"), r#"{"n":4}"#, ""),
    c("model.favorite", "Use favourite model 5", "box", Some("⌃ ⌘ 5"), r#"{"n":5}"#, ""),
    c("model.effort", "Think harder", "gauge", Some("⇧ ⌘ E"), "{}", "effort?: low|medium|high|xhigh|max|default; without one, the next level up; session?"),
    c("model.fast", "Fast mode on or off", "zap", Some("⌥ ⌘ F"), "{}", "fast?: true|false; without it, flips; session?"),
    q("model.list", "The models and the favourites", ""),
    q("model.favorites", "Replace the favourites", "models: [{model, effort?, fast?}], at most 5"),
    c("session.mode", "Mode: Ask first", "shield", None, r#"{"mode":"default"}"#, "mode: default|acceptEdits|auto|plan, session?"),
    c("session.mode", "Mode: Accept edits", "pencil", None, r#"{"mode":"acceptEdits"}"#, ""),
    c("session.mode", "Mode: Auto", "zap", None, r#"{"mode":"auto"}"#, ""),
    c("session.mode", "Mode: Plan", "list-checks", None, r#"{"mode":"plan"}"#, ""),
    // panes
    c("pane.split", "Split right", "columns-2", Some("⌘ D"), r#"{"dir":"right"}"#, "dir: right|down, pane?"),
    c("pane.split", "Split down", "rows-2", Some("⇧ ⌘ D"), r#"{"dir":"down"}"#, ""),
    c("pane.zoom", "Maximize or restore pane", "maximize-2", Some("⇧ ⌘ ↩"), "{}", "pane?"),
    c("pane.close", "Close pane", "x", Some("⌘ W"), "{}", "pane?"),
    c("pane.show", "Show Conversation here", "message-square", None, r#"{"module":"conversation"}"#, "module, pane?"),
    c("pane.show", "Show Sessions here", "list", None, r#"{"module":"sessions"}"#, ""),
    c("pane.show", "Show Workspace here", "panels-top-left", None, r#"{"module":"workspace"}"#, ""),
    c("pane.show", "Show Changes here", "git-compare", None, r#"{"module":"changes"}"#, ""),
    c("pane.show", "Show Files here", "folder", None, r#"{"module":"files"}"#, ""),
    c("pane.show", "Show Checks here", "circle-check", None, r#"{"module":"checks"}"#, ""),
    c("pane.show", "Show Artifacts here", "sparkles", None, r#"{"module":"artifacts"}"#, ""),
    c("pane.show", "Show Info here", "info", None, r#"{"module":"info"}"#, ""),
    c("pane.show", "Show Surfaces here", "layout-dashboard", None, r#"{"module":"surfaces"}"#, ""),
    c("pane.show", "Show Terminal here", "square-terminal", None, r#"{"module":"terminal"}"#, ""),
    c("pane.show", "Show Git here", "git-graph", None, r#"{"module":"git"}"#, ""),
    q("files.open", "Open a file", "path: relative to the session's folder"),
    c("terminal.new", "New terminal", "square-terminal", Some("⌃ `"), "{}", "opens a shell below the focused pane; answers with its pane"),
    q("pane.focus", "Focus a pane", "pane"),
    q("pane.toggle", "Show or hide a pane", "pane"),
    q("pane.move", "Move a pane", "pane, to, zone: left|right|top|bottom|swap"),
    c("workspace.tab", "Workspace: Changes", "git-compare", None, r#"{"tab":"changes"}"#, "tab: changes|files|checks|artifacts|info"),
    c("workspace.tab", "Workspace: Files", "folder", None, r#"{"tab":"files"}"#, ""),
    c("workspace.tab", "Workspace: Checks", "circle-check", None, r#"{"tab":"checks"}"#, ""),
    // look
    c("theme.set", "Theme: Tessel", "palette", None, r#"{"base":"tessel"}"#, "base: tessel|orchard|tern"),
    c("theme.set", "Theme: Orchard", "palette", None, r#"{"base":"orchard"}"#, ""),
    c("theme.set", "Theme: Tern", "palette", None, r#"{"base":"tern"}"#, ""),
    c("theme.appearance", "Appearance: System", "sun-moon", None, r#"{"mode":"system"}"#, "mode: system|light|dark"),
    c("theme.appearance", "Appearance: Light", "sun", None, r#"{"mode":"light"}"#, ""),
    c("theme.appearance", "Appearance: Dark", "moon", None, r#"{"mode":"dark"}"#, ""),
    c("theme.size", "Interface size: Compact", "a-large-small", None, r#"{"size":"compact"}"#, "size: compact|default|large"),
    c("theme.size", "Interface size: Default", "a-large-small", None, r#"{"size":"default"}"#, ""),
    c("theme.size", "Interface size: Large", "a-large-small", None, r#"{"size":"large"}"#, ""),
    q("theme.accent", "Set the accent colour", "hex: #rrggbb, or null for the theme's own"),
    q("theme.use", "Use a theme", "name: one of theme.list, or a file in ~/.tessel/themes"),
    q("theme.list", "Every theme", ""),
    c("theme.density", "Density: Islands", "layout-grid", None, r#"{"density":"islands"}"#, "density: islands|compact"),
    c("theme.density", "Density: Compact", "layout-panel-left", None, r#"{"density":"compact"}"#, ""),
    c("theme.material", "Material: Solid", "square", None, r#"{"material":"solid"}"#, "material: solid|glass"),
    c("theme.material", "Material: Glass", "app-window", None, r#"{"material":"glass"}"#, ""),
    c("layout.use", "Layout: Studio", "panels-top-left", None, r#"{"name":"studio"}"#, "name: studio|focus|tiles|rail"),
    c("layout.use", "Layout: Focus", "focus", None, r#"{"name":"focus"}"#, ""),
    c("layout.use", "Layout: Tiles", "layout-dashboard", None, r#"{"name":"tiles"}"#, ""),
    c("layout.use", "Layout: Rail", "layout-panel-left", None, r#"{"name":"rail"}"#, ""),
    // the daemon
    c("daemon.set", "Agents: let them use tessel", "bot", None, r#"{"agent_tools":true}"#, "agent_tools?: true|false. tessel's tools for agents, as each next starts"),
    c("daemon.set", "Agents: no tessel tools", "bot-off", None, r#"{"agent_tools":false}"#, ""),
    // the layout file, whole
    c("layout.reset", "Reset the layout", "rotate-ccw", None, "{}", ""),
    q("layout.get", "The layout file (ui.json)", ""),
    q("layout.set", "Replace the layout file", "json: the whole ui.json; answers with what check.txt says"),
    // looking
    q("ui.ls", "What the window shows", ""),
    q("commands.list", "Every command", ""),
];

/// the entry for an id: the first, which carries its argument help.
pub fn spec(id: &str) -> Option<&'static Spec> {
    COMMANDS.iter().find(|s| s.id == id)
}

/// how well `query` matches `text`: every query character in order, more for
/// starts of words and runs. `None` if it doesn't match at all.
pub fn score(query: &str, text: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let mut score = 0;
    let mut qi = query.chars().map(|c| c.to_ascii_lowercase()).peekable();
    let mut prev_matched = false;
    let mut prev: Option<char> = None;
    for (i, ch) in text.chars().enumerate() {
        let Some(&want) = qi.peek() else { break };
        if ch.to_ascii_lowercase() == want {
            qi.next();
            let word_start = prev.is_none_or(|p| !p.is_alphanumeric()) || (p_is_lower(prev) && ch.is_uppercase());
            score += 1 + if word_start { 8 } else { 0 } + if prev_matched { 4 } else { 0 } - (i as i32 / 8);
            prev_matched = true;
        } else {
            prev_matched = false;
        }
        prev = Some(ch);
    }
    if qi.peek().is_some() {
        return None;
    }
    // what you typed, whole, beats the same letters scattered: more at the
    // start of a word, more the earlier it is, and short titles before long.
    let (lower_text, lower_query) = (text.to_lowercase(), query.to_lowercase());
    if let Some(at) = lower_text.find(&lower_query) {
        let word_start = at == 0 || !lower_text[..at].ends_with(|c: char| c.is_alphanumeric());
        score += 100 + if word_start { 50 } else { 0 } - at as i32;
    }
    Some(score - text.len() as i32 / 4)
}

fn p_is_lower(c: Option<char>) -> bool {
    c.is_some_and(|c| c.is_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{COMMANDS, score, spec};

    #[test]
    fn every_command_is_complete_and_every_preset_parses() {
        let reference = crate::ui::REFERENCE;
        for s in COMMANDS {
            assert!(!s.title.is_empty() && !s.icon.is_empty(), "{}", s.id);
            assert!(serde_json::from_str::<serde_json::Value>(s.preset).is_ok_and(|v| v.is_object()), "{} preset {}", s.id, s.preset);
            assert!(reference.contains(&format!("`{}`", s.id)), "the reference documents {}", s.id);
            // the first entry of an id carries its argument help.
            if !spec(s.id).is_some_and(|first| std::ptr::eq(first, s)) {
                assert!(s.args.is_empty(), "{}: argument help belongs on its first entry", s.id);
            }
        }
        let mut keys: Vec<&str> = COMMANDS.iter().filter_map(|s| s.keys).collect();
        let n = keys.len();
        keys.sort_unstable();
        keys.dedup();
        assert_eq!(keys.len(), n, "two commands share a shortcut");
    }

    #[test]
    fn search_prefers_word_starts_and_runs() {
        assert!(score("sr", "Split right").unwrap() > score("sr", "Show Sessions here").unwrap_or(i32::MIN));
        assert!(score("split", "Split down").is_some());
        assert!(score("xyz", "Split right").is_none());
        assert!(score("", "anything") == Some(0));
        assert!(score("thm", "Theme: Tern").is_some());
        assert!(score("sessions", "All sessions").unwrap() > score("sessions", "Show Sessions here").unwrap(), "whole and early wins");
    }
}
