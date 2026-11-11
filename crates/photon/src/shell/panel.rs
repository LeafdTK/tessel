//! the workspace panel: what the open session has done to its folder.
//!
//! five tabs beside the transcript. changes (what differs from the base
//! branch, with the diff), files (the folder, with an editor), checks (the
//! branch, its pull request and ci), artifacts (plans and files the agent
//! produced) and info (the session itself). everything here is asked of the
//! daemon, so it works the same when the daemon is on another machine.

use super::*;
use gpui_kit::component::diff::{Diff, DiffFile, DiffLineRange, DiffMode, DiffState};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::input::{Editor, EditorState};
use photon_proto::{ChangeStatus, CheckState, DirEntry, FileContent, FileDiff, GitSummary, PrStatus};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tab {
    #[default]
    Changes,
    Files,
    Checks,
    Artifacts,
    Info,
}

impl Tab {
    pub(crate) const ALL: [Tab; 5] = [Tab::Changes, Tab::Files, Tab::Checks, Tab::Artifacts, Tab::Info];

    pub(crate) fn title(self) -> &'static str {
        match self {
            Tab::Changes => "Changes",
            Tab::Files => "Files",
            Tab::Checks => "Checks",
            Tab::Artifacts => "Artifacts",
            Tab::Info => "Info",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Tab::Changes => "tab-changes",
            Tab::Files => "tab-files",
            Tab::Checks => "tab-checks",
            Tab::Artifacts => "tab-artifacts",
            Tab::Info => "tab-info",
        }
    }
}

/// a file open in the editor.
pub(crate) struct OpenFile {
    pub(crate) path: String,
    /// none when there is nothing to edit (a binary file).
    pub(crate) editor: Option<Entity<EditorState>>,
    /// the text as it is on disk, as far as we know.
    pub(crate) saved: String,
    pub(crate) dirty: bool,
    /// why it can't be edited, if it can't.
    pub(crate) note: Option<String>,
    /// what was sent to be saved and has not been confirmed yet.
    saving: Option<String>,
    _watch: Option<Subscription>,
}

/// everything the panel shows for one session. kept per session, so going
/// away and coming back finds the same tab, the same file, and any edits.
#[derive(Default)]
pub(crate) struct Workspace {
    pub(crate) tab: Tab,
    pub(crate) git: Option<GitSummary>,
    /// the changed file whose diff is showing.
    pub(crate) selected: Option<String>,
    pub(crate) diff: Option<Entity<DiffState>>,
    /// shown in place of a diff: binary, too large, or nothing changed.
    pub(crate) diff_note: Option<String>,
    pub(crate) split: bool,
    /// folders we have listed, by path from the session's folder ("" is the top).
    pub(crate) dirs: HashMap<String, Vec<DirEntry>>,
    pub(crate) expanded: HashSet<String>,
    pub(crate) file: Option<OpenFile>,
    /// a file that has arrived but has no editor yet (making one needs the window).
    incoming: Option<FileContent>,
    pub(crate) pr: Option<PrStatus>,
    pub(crate) pr_loading: bool,
    /// the agent touched the folder since we last looked.
    pub(crate) stale: bool,
}

/// one line of the file tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct TreeRow {
    pub(crate) path: String,
    pub(crate) name: String,
    pub(crate) depth: usize,
    pub(crate) is_dir: bool,
    pub(crate) open: bool,
}

/// more rows than this and the tree stops: nobody scrolls that far, and a
/// `node_modules` opened by accident must not stall the window.
const TREE_MAX: usize = 3000;

impl Workspace {
    /// the tree as shown: each open folder's contents under it.
    pub(crate) fn tree(&self) -> Vec<TreeRow> {
        let mut out = Vec::new();
        self.walk("", 0, &mut out);
        out
    }

    fn walk(&self, dir: &str, depth: usize, out: &mut Vec<TreeRow>) {
        let Some(entries) = self.dirs.get(dir) else { return };
        for e in entries {
            if out.len() >= TREE_MAX {
                return;
            }
            let path = if dir.is_empty() { e.name.clone() } else { format!("{dir}/{}", e.name) };
            let open = e.is_dir && self.expanded.contains(&path);
            out.push(TreeRow { path: path.clone(), name: e.name.clone(), depth, is_dir: e.is_dir, open });
            if open {
                self.walk(&path, depth + 1, out);
            }
        }
    }
}

/// a plan the agent wrote, or a file it created.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Artifact {
    Plan { id: String, text: String },
    File { path: String },
}

/// what the agent has produced in this session, newest first: its plans, and
/// the files it created (as opposed to edited).
pub(crate) fn artifacts(open: &Open, cwd: &str) -> Vec<Artifact> {
    let mut out: Vec<Artifact> = Vec::new();
    for row in 0..open.len() {
        let Some((item, _)) = open.row(row) else { continue };
        let ItemBody::Tool { tool, input, status, .. } = &item.body else { continue };
        match tool.as_str() {
            "ExitPlanMode" => {
                if let Some(text) = input.get("plan").and_then(|v| v.as_str()).filter(|t| !t.trim().is_empty()) {
                    out.push(Artifact::Plan { id: item.id.to_string(), text: text.to_owned() });
                }
            }
            "Write" if *status == ToolStatus::Ok => {
                let Some(path) = input.get("file_path").and_then(|v| v.as_str()) else { continue };
                // the agent keeps its own copy of each plan in its settings folder: that is the plan above, not a deliverable.
                if path.contains("/.claude/plans/") {
                    continue;
                }
                let path = relative_to(path, cwd);
                out.retain(|a| !matches!(a, Artifact::File { path: p } if *p == path));
                out.push(Artifact::File { path });
            }
            _ => {}
        }
    }
    out.reverse();
    out
}

/// a path as the session's folder sees it, when it is inside that folder.
pub(crate) fn relative_to(path: &str, cwd: &str) -> String {
    let cwd = cwd.trim_end_matches('/');
    match path.strip_prefix(cwd).and_then(|rest| rest.strip_prefix('/')) {
        Some(rest) if !rest.is_empty() => rest.to_owned(),
        _ => path.to_owned(),
    }
}

/// the editor's name for a file's language, from its extension.
fn language_of(path: &str) -> &'static str {
    match path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase().as_str() {
        "rs" => "rust",
        "ts" | "tsx" => "typescript",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        "py" => "python",
        "go" => "go",
        "rb" => "ruby",
        "json" => "json",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "md" | "markdown" => "markdown",
        "html" | "htm" => "html",
        "css" => "css",
        "sh" | "bash" | "zsh" => "bash",
        "sql" => "sql",
        "c" | "h" => "c",
        "cpp" | "cc" | "hpp" => "cpp",
        "swift" => "swift",
        "java" => "java",
        _ => "text",
    }
}

fn split_path(path: &str) -> (&str, &str) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name, dir),
        None => (path, ""),
    }
}

impl Shell {
    fn open_id(&self) -> Option<SessionId> {
        self.model.open.as_ref().map(|o| o.id).filter(|_| !self.show_new)
    }

    /// make the panel describe the open session, keeping what it showed for
    /// the previous one for when you come back.
    pub(crate) fn switch_workspace(&mut self) {
        let next = self.open_id();
        if next == self.ws_session {
            return;
        }
        let current = std::mem::take(&mut self.ws);
        let tab = current.tab;
        if let Some(prev) = self.ws_session {
            self.ws_parked.insert(prev, current);
        }
        self.ws = next.and_then(|id| self.ws_parked.remove(&id)).unwrap_or_else(|| Workspace { tab, ..Workspace::default() });
        self.ws_session = next;
        self.refresh_workspace();
    }

    pub(crate) fn set_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.ws.tab = tab;
        self.refresh_workspace();
        cx.notify();
    }

    /// ask the daemon for what the visible tabs show. nothing visible asks for nothing.
    pub(crate) fn refresh_workspace(&mut self) {
        let Some(session) = self.ws_session else { return };
        let tabs = self.visible_tabs();
        if tabs.is_empty() {
            return;
        }
        self.ws.stale = false;
        let mut asked_git = false;
        for tab in tabs {
            self.refresh_tab(session, tab, &mut asked_git);
        }
    }

    fn refresh_tab(&mut self, session: SessionId, tab: Tab, asked_git: &mut bool) {
        match tab {
            Tab::Changes => {
                if !std::mem::replace(asked_git, true) {
                    self.cmd(Cmd::Git(session));
                }
                if let Some(path) = self.ws.selected.clone() {
                    self.cmd(Cmd::Diff { session, path });
                }
            }
            Tab::Files => {
                self.cmd(Cmd::List { session, path: String::new() });
                for path in self.ws.expanded.clone() {
                    self.cmd(Cmd::List { session, path });
                }
            }
            Tab::Checks => {
                if !std::mem::replace(asked_git, true) {
                    self.cmd(Cmd::Git(session));
                }
                self.ws.pr_loading = true;
                self.cmd(Cmd::Pr(session));
            }
            Tab::Artifacts => {}
            Tab::Info => self.cmd(Cmd::RefreshMcp(session)),
        }
    }

    /// the agent did something that may have changed its folder.
    pub(crate) fn workspace_touched(&mut self) {
        self.ws.stale = true;
    }

    /// once a second: catch up with the folder if the agent has been at it.
    pub(crate) fn workspace_tick(&mut self) {
        if self.ws.stale && self.panel_visible() {
            self.refresh_workspace();
        }
    }

    pub(crate) fn apply_git(&mut self, session: SessionId, summary: GitSummary) {
        if self.ws_session != Some(session) {
            return;
        }
        let still_there = self.ws.selected.as_ref().is_some_and(|sel| summary.files.iter().any(|f| &f.path == sel));
        if !still_there {
            // show something straight away: the first change is as good a start as any.
            self.ws.selected = summary.files.first().map(|f| f.path.clone());
            self.ws.diff = None;
            self.ws.diff_note = None;
            if let Some(path) = self.ws.selected.clone().filter(|_| self.ws.tab == Tab::Changes) {
                self.cmd(Cmd::Diff { session, path });
            }
        }
        self.ws.git = Some(summary);
    }

    pub(crate) fn apply_diff(&mut self, session: SessionId, diff: FileDiff, cx: &mut Context<Self>) {
        if self.ws_session != Some(session) || self.ws.selected.as_deref() != Some(diff.path.as_str()) {
            return;
        }
        let files = if diff.binary { Vec::new() } else { DiffFile::parse(&diff.unified).unwrap_or_default() };
        self.ws.diff_note = if diff.binary {
            Some("This is a binary file.".into())
        } else if files.is_empty() {
            Some("Nothing differs from the base branch.".into())
        } else if diff.truncated {
            Some("This file is too large to show in full.".into())
        } else {
            None
        };
        if files.is_empty() {
            self.ws.diff = None;
            return;
        }
        match &self.ws.diff {
            Some(state) => state.update(cx, |s, cx| s.set_files(files, cx)),
            None => {
                let mode = if self.ws.split { DiffMode::Split } else { DiffMode::Unified };
                self.ws.diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(mode)));
            }
        }
    }

    pub(crate) fn apply_dir(&mut self, session: SessionId, path: String, entries: Vec<DirEntry>) {
        if self.ws_session == Some(session) {
            self.ws.dirs.insert(path, entries);
        }
    }

    pub(crate) fn apply_file(&mut self, session: SessionId, file: FileContent) {
        if self.ws_session == Some(session) {
            self.ws.incoming = Some(file);
        }
    }

    pub(crate) fn apply_saved(&mut self, session: SessionId, path: String) {
        if self.ws_session != Some(session) {
            return;
        }
        if let Some(file) = self.ws.file.as_mut().filter(|f| f.path == path)
            && let Some(sent) = file.saving.take()
        {
            file.saved = sent;
        }
        // whatever was typed while the save was on its way still counts as unsaved.
        self.ws.stale = true;
    }

    pub(crate) fn apply_pr(&mut self, session: SessionId, status: PrStatus) {
        if self.ws_session == Some(session) {
            self.ws.pr = Some(status);
            self.ws.pr_loading = false;
        }
    }

    /// give a file that has arrived its editor.
    pub(crate) fn materialize_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.ws.incoming.take() else { return };
        let text = f.text.clone().unwrap_or_default();
        let note = if f.binary {
            Some("This is a binary file, so there is nothing to edit.".to_owned())
        } else if f.truncated {
            Some("Too large to edit here. Showing the beginning, read-only.".to_owned())
        } else {
            None
        };
        let (editor, watch) = if f.binary {
            (None, None)
        } else {
            let language = language_of(&f.path);
            let value = text.clone();
            let editor = cx.new(|cx| EditorState::new(window, cx).language(language).line_number(true).soft_wrap(false).default_value(value));
            let watch = cx.subscribe(&editor, |this, editor, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change)
                    && let Some(file) = this.ws.file.as_mut()
                {
                    let dirty = editor.read(cx).value().as_ref() != file.saved.as_str();
                    if dirty != file.dirty {
                        file.dirty = dirty;
                        cx.notify();
                    }
                }
            });
            (Some(editor), Some(watch))
        };
        self.ws.file = Some(OpenFile { path: f.path, editor, saved: text, dirty: false, note, saving: None, _watch: watch });
    }

    pub(crate) fn select_change(&mut self, path: String, cx: &mut Context<Self>) {
        if let Some(session) = self.ws_session {
            self.ws.selected = Some(path.clone());
            self.ws.diff = None;
            self.ws.diff_note = None;
            self.cmd(Cmd::Diff { session, path });
        }
        cx.notify();
    }

    pub(crate) fn toggle_split(&mut self, cx: &mut Context<Self>) {
        self.ws.split = !self.ws.split;
        let mode = if self.ws.split { DiffMode::Split } else { DiffMode::Unified };
        if let Some(state) = &self.ws.diff {
            state.update(cx, |s, cx| s.set_mode(mode, cx));
        }
        cx.notify();
    }

    pub(crate) fn toggle_dir(&mut self, path: String, cx: &mut Context<Self>) {
        if !self.ws.expanded.remove(&path) {
            if !self.ws.dirs.contains_key(&path)
                && let Some(session) = self.ws_session
            {
                self.cmd(Cmd::List { session, path: path.clone() });
            }
            self.ws.expanded.insert(path);
        }
        cx.notify();
    }

    /// open a file in the editor. unsaved edits to another file are never dropped quietly.
    pub(crate) fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        let Some(session) = self.ws_session else { return };
        match &self.ws.file {
            Some(f) if f.path == path => {}
            Some(f) if f.dirty => self.error = Some(format!("Save or revert your changes to {} first.", split_path(&f.path).0)),
            _ => {
                self.error = None;
                self.ws.tab = Tab::Files;
                // open the folders above it, so the tree shows where it is.
                let mut dir = String::new();
                for part in path.split('/').collect::<Vec<_>>().split_last().map_or(&[][..], |(_, dirs)| dirs) {
                    dir = if dir.is_empty() { (*part).to_owned() } else { format!("{dir}/{part}") };
                    if self.ws.expanded.insert(dir.clone()) && !self.ws.dirs.contains_key(&dir) {
                        self.cmd(Cmd::List { session, path: dir.clone() });
                    }
                }
                self.cmd(Cmd::Read { session, path });
            }
        }
        cx.notify();
    }

    pub(crate) fn save_file(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.ws_session else { return };
        let Some(file) = self.ws.file.as_mut().filter(|f| f.dirty && f.note.is_none()) else { return };
        let Some(editor) = &file.editor else { return };
        let content = editor.read(cx).value().to_string();
        file.saving = Some(content.clone());
        file.dirty = false;
        let path = file.path.clone();
        self.cmd(Cmd::Write { session, path, content });
        cx.notify();
    }

    pub(crate) fn revert_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(file) = self.ws.file.as_mut()
            && let Some(editor) = &file.editor
        {
            let saved = file.saved.clone();
            editor.update(cx, |e, cx| e.set_value(saved, window, cx));
            file.dirty = false;
        }
        cx.notify();
    }

    pub(crate) fn close_file(&mut self, cx: &mut Context<Self>) {
        match &self.ws.file {
            Some(f) if f.dirty => self.error = Some(format!("Save or revert your changes to {} first.", split_path(&f.path).0)),
            _ => self.ws.file = None,
        }
        cx.notify();
    }

    /// point the next message at some lines of a diff, ready for you to say what's wrong with them.
    pub(crate) fn comment_on(&mut self, range: &DiffLineRange, window: &mut Window, cx: &mut Context<Self>) {
        let lines = if range.start() == range.end() { format!("{}", range.start()) } else { format!("{}-{}", range.start(), range.end()) };
        let reference = format!("`{}:{lines}` ", range.path());
        self.composer.update(cx, |c, cx| {
            c.insert(reference, window, cx);
            c.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn link_folder(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.ws_session else { return };
        let pick = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: true, prompt: Some("Link".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else { return };
            let _ = this.update(cx, |this, _| {
                for path in paths {
                    this.cmd(Cmd::LinkDir { session, path: path.to_string_lossy().into_owned() });
                }
            });
        })
        .detach();
    }

    // ---- drawing ---------------------------------------------------------------

    pub(crate) fn panel(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let tabs = self.panel_tabs_el(cx).h(px(44.)).flex_shrink_0().px_2().border_b_1().border_color(p.separator);
        v_flex().size_full().overflow_hidden().child(tabs).child(self.panel_body(cx))
    }

    /// the workspace's tabs, as a row of pills.
    pub(crate) fn panel_tabs_el(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let mut tabs = h_flex().gap_1();
        for tab in self.panel_tabs.clone() {
            let selected = self.ws.tab == tab;
            let count = match tab {
                Tab::Changes => self.ws.git.as_ref().filter(|g| g.is_repo && !g.files.is_empty()).map(|g| g.files.len()),
                _ => None,
            };
            let icon = match tab {
                Tab::Changes => "git-compare",
                Tab::Files => "folder",
                Tab::Checks => "circle-check",
                Tab::Artifacts => "sparkles",
                Tab::Info => "info",
            };
            // the chosen tab says what it is; the rest are icons, so the row fits any pane.
            tabs = tabs.child(
                h_flex()
                    .id(tab.id())
                    .test_support()
                    .aria_label(tab.title())
                    .h(px(28.))
                    .px(px(if selected { 10. } else { 8. }))
                    .gap_1p5()
                    .flex_shrink_0()
                    .rounded(px(theme::R_TILE))
                    .set(Type::Callout)
                    .when(selected, |d| d.bg(p.selection).text_color(p.label))
                    .when(!selected, |d| d.text_color(p.secondary).hover(|d| d.bg(p.hover).text_color(p.label)))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tab.title()).build(window, cx))
                    .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
                    .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).small())
                    .when(selected, |d| d.child(tab.title()).children(count.map(|n| div().text_color(p.tertiary).child(n.to_string())))),
            );
        }
        tabs
    }

    /// what the chosen tab shows.
    pub(crate) fn panel_body(&self, cx: &mut Context<Self>) -> Div {
        let col = div().flex_1().min_h_0().overflow_hidden();
        let Some(s) = self.model.open_summary() else { return col };
        let body = match self.ws.tab {
            Tab::Changes => self.changes_tab(cx).into_any_element(),
            Tab::Files => self.files_tab(cx).into_any_element(),
            Tab::Checks => self.checks_tab(cx).into_any_element(),
            Tab::Artifacts => self.artifacts_tab(s, cx).into_any_element(),
            Tab::Info => self.info_tab(s, cx).into_any_element(),
        };
        col.child(body)
    }

    fn quiet(&self, text: impl Into<SharedString>) -> Div {
        div().px_3().py_3().set(Type::Subheadline).text_color(self.palette.get().secondary).child(text.into())
    }

    fn section(&self, title: &'static str) -> Div {
        div().px_3().pt_4().pb_1().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(self.palette.get().secondary).child(title)
    }

    pub(crate) fn changes_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let col = v_flex().size_full();
        let Some(git) = &self.ws.git else { return col.child(self.quiet("Looking at the folder…")) };
        if !git.is_repo {
            return col.child(self.quiet("This folder is not a git repository, so there is nothing to compare against."));
        }
        let mono = theme::mono();
        // what was added and taken away, in the colours diffs use. nothing is said about a zero.
        let counts = move |added: u32, removed: u32| {
            h_flex()
                .flex_shrink_0()
                .gap_1()
                .font_family(mono.clone())
                .text_size(px(11.))
                .when(added > 0, |d| d.child(div().text_color(p.finished).child(format!("+{}", thousands(u64::from(added))))))
                .when(removed > 0, |d| d.child(div().text_color(p.failed).child(format!("−{}", thousands(u64::from(removed))))))
        };
        let since = git.base.as_deref().map(|b| format!(" since {}", short_ref(b))).unwrap_or_default();
        let header = h_flex()
            .h(px(32.))
            .px_3()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(p.separator)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .set(Type::Subheadline)
                    .text_color(p.secondary)
                    .child(format!("{} {} changed", git.files.len(), if git.files.len() == 1 { "file" } else { "files" }))
                    .child(div().text_color(p.tertiary).child(since)),
            )
            .child(counts(git.added, git.removed))
            .child(
                Button::new("diff-mode")
                    .xsmall()
                    .ghost()
                    .icon(gpui_kit::component::Icon::empty().path(if self.ws.split { "icons/columns-2.svg" } else { "icons/rows-2.svg" }))
                    .tooltip(if self.ws.split { "Side by Side. Click for Unified." } else { "Unified. Click for Side by Side." })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_split(cx))),
            );
        if git.files.is_empty() {
            return col.child(header).child(self.quiet("Nothing has changed yet."));
        }
        let mut list = v_flex().id("changes-list").flex_shrink_0().max_h(relative(0.45)).overflow_y_scroll();
        for f in &git.files {
            let selected = self.ws.selected.as_deref() == Some(f.path.as_str());
            let (name, dir) = split_path(&f.path);
            let (letter, glyph, tint) = match f.status {
                ChangeStatus::Added | ChangeStatus::Untracked => ("A", "icons/square-plus.svg", p.finished),
                ChangeStatus::Modified => ("M", "icons/square-dot.svg", p.secondary),
                ChangeStatus::Deleted => ("D", "icons/square-minus.svg", p.failed),
                ChangeStatus::Unknown => ("?", "icons/square.svg", p.tertiary),
            };
            let path = f.path.clone();
            list = list.child(
                h_flex()
                    .id(ElementId::from(SharedString::from(format!("change-{}", f.path))))
                    .test_support()
                    .aria_label(SharedString::from(format!("{}, {} added, {} removed", f.path, f.added, f.removed)))
                    .h(px(28.))
                    .px_3()
                    .gap_2()
                    .border_b_1()
                    .border_color(p.separator.opacity(0.6))
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|d| d.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| this.select_change(path.clone(), cx)))
                    // the folder quietly, then the name: one line that reads as a path.
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .set(Type::Code)
                            .child(div().min_w_0().flex_shrink(1.0).truncate().text_color(p.tertiary).child(if dir.is_empty() { String::new() } else { format!("{dir}/") }))
                            .child(div().flex_shrink_0().max_w(relative(0.75)).truncate().text_color(p.label).child(name.to_owned())),
                    )
                    .child(if f.binary { div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child("binary").into_any_element() } else { counts(f.added, f.removed).into_any_element() })
                    .child(div().w(px(10.)).flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).child(letter))
                    .child(div().flex_shrink_0().text_color(tint).child(gpui_kit::component::Icon::empty().path(glyph).small())),
            );
        }
        let me = cx.entity().downgrade();
        let diff: AnyElement = match (&self.ws.diff, &self.ws.diff_note) {
            (Some(state), _) => Diff::new(state)
                .size_full()
                // the pane is the frame. the diff's own square border would
                // double its edge and poke out of its rounded corners.
                .border_0()
                .on_add_annotation(move |range, window, cx| {
                    let _ = me.update(cx, |this, cx| this.comment_on(range, window, cx));
                })
                .into_any_element(),
            (None, Some(note)) => self.quiet(note.clone()).into_any_element(),
            (None, None) => self.quiet("Loading the diff…").into_any_element(),
        };
        col.child(header).child(list).child(div().h(px(1.)).flex_shrink_0().bg(p.separator)).child(div().id("diff-view").test_support().aria_label("Diff").flex_1().min_h_0().overflow_hidden().child(diff))
    }

    pub(crate) fn files_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let col = v_flex().size_full();
        if !self.ws.dirs.contains_key("") {
            return col.child(self.quiet("Reading the folder…"));
        }
        let rows = self.ws.tree();
        let showing_file = self.ws.file.is_some();
        let mut tree = v_flex().id("files-tree").overflow_y_scroll().py_1();
        tree = if showing_file { tree.flex_shrink_0().h(px(220.)) } else { tree.flex_1().min_h_0() };
        if rows.is_empty() {
            tree = tree.child(self.quiet("This folder is empty."));
        }
        let full = rows.len() >= TREE_MAX;
        for row in rows {
            let selected = self.ws.file.as_ref().is_some_and(|f| f.path == row.path);
            let path = row.path.clone();
            let is_dir = row.is_dir;
            tree = tree.child(
                h_flex()
                    .id(ElementId::from(SharedString::from(format!("file-{}", row.path))))
                    .test_support()
                    .aria_label(SharedString::from(row.path.clone()))
                    .h(px(24.))
                    .mx_1()
                    .pr_2()
                    .pl(px(8. + 14. * row.depth as f32))
                    .gap_1()
                    .rounded(px(theme::R_TILE))
                    .when(selected, |d| d.bg(p.selection))
                    .when(!selected, |d| d.hover(|d| d.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| if is_dir { this.toggle_dir(path.clone(), cx) } else { this.open_file(path.clone(), cx) }))
                    .child(div().w(px(12.)).flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(match (row.is_dir, row.open) {
                        (true, true) => "▾",
                        (true, false) => "▸",
                        _ => "",
                    }))
                    .child(div().flex_1().overflow_hidden().set(Type::Callout).text_color(if row.is_dir { p.secondary } else { p.label }).truncate().child(row.name)),
            );
        }
        if full {
            tree = tree.child(self.quiet("There is more here than fits. Close a folder to see the rest."));
        }
        let Some(file) = &self.ws.file else { return col.child(tree) };
        let (name, dir) = split_path(&file.path);
        let editable = file.editor.is_some() && file.note.is_none();
        let header = h_flex()
            .h(px(34.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_t_1()
            .border_b_1()
            .border_color(p.separator)
            .child(div().flex_shrink_0().set(Type::Callout).text_color(p.label).child(name.to_owned()))
            .child(div().flex_1().overflow_hidden().set(Type::Subheadline).text_color(p.tertiary).truncate().child(dir.to_owned()))
            .when(file.dirty, |d| {
                d.child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).child("Edited"))
                    .child(Button::new("file-revert").xsmall().ghost().label("Revert").on_click(cx.listener(|this, _, window, cx| this.revert_file(window, cx))))
            })
            .when(editable, |d| {
                d.child(Button::new("file-save").xsmall().when(file.dirty, |b| b.primary()).when(!file.dirty, |b| b.outline()).disabled(!file.dirty).label("Save").on_click(cx.listener(|this, _, _, cx| this.save_file(cx))))
            })
            .child(Button::new("file-close").xsmall().ghost().label("Close").on_click(cx.listener(|this, _, _, cx| this.close_file(cx))));
        let body: AnyElement = match &file.editor {
            Some(editor) => div().size_full().child(Editor::new(editor).h(relative(1.)).bordered(false).readonly(!editable)).into_any_element(),
            None => div().into_any_element(),
        };
        col.child(tree)
            .child(header)
            .children(file.note.clone().map(|n| self.quiet(n)))
            .child(div().id("file-editor").test_support().aria_label("File contents").flex_1().min_h_0().overflow_hidden().child(body))
    }

    pub(crate) fn checks_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let line = |text: String| div().px_3().set(Type::Callout).text_color(p.label).truncate().child(text);
        let dim = |text: String| div().px_3().set(Type::Subheadline).text_color(p.secondary).child(text);
        let mut col = v_flex().id("checks").size_full().overflow_y_scroll().pb_4();

        col = col.child(self.section("Branch"));
        match &self.ws.git {
            None => col = col.child(dim("Looking…".into())),
            Some(g) if !g.is_repo => col = col.child(dim("Not a git repository.".into())),
            Some(g) => {
                col = col.child(line(g.branch.clone().unwrap_or_else(|| "Detached".into())));
                if let Some(base) = &g.base {
                    let ahead = match g.ahead {
                        0 => "no commits".to_owned(),
                        1 => "1 commit".to_owned(),
                        n => format!("{n} commits"),
                    };
                    col = col.child(dim(format!("{ahead} ahead of {}", short_ref(base))));
                }
                col = col.child(dim(format!("{} files changed, +{} −{}", g.files.len(), g.added, g.removed)));
            }
        }

        col = col.child(
            h_flex().pr_3().items_end().child(self.section("Pull Request").flex_1()).child(
                Button::new("checks-refresh")
                    .xsmall()
                    .ghost()
                    .label(if self.ws.pr_loading { "Checking…" } else { "Refresh" })
                    .disabled(self.ws.pr_loading)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.refresh_workspace();
                        cx.notify();
                    })),
            ),
        );
        let Some(status) = &self.ws.pr else { return col.child(dim("Asking GitHub…".into())) };
        let Some(pr) = &status.pr else {
            return col.child(dim(status.note.clone().unwrap_or_else(|| "No pull request for this branch yet.".into())));
        };
        let url = pr.url.clone();
        col = col
            .child(
                div()
                    .id("pr-link")
                    .test_support()
                    .aria_label(SharedString::from(format!("Open pull request {}", pr.number)))
                    .px_3()
                    .set(Type::Callout)
                    .text_color(p.label)
                    .hover(|d| d.text_color(p.accent))
                    .on_click(move |_, _, cx| cx.open_url(&url))
                    .child(format!("#{} {}", pr.number, pr.title)),
            )
            .child(dim(format!("{}{} · {} → {}", if pr.draft { "Draft · " } else { "" }, sentence(&pr.state.to_lowercase()), pr.head, pr.base)));

        col = col.child(self.section("Checks"));
        if pr.checks.is_empty() {
            col = col.child(dim("No checks have reported.".into()));
        }
        let failing = pr.checks.iter().filter(|c| c.state == CheckState::Failing).count();
        let pending = pr.checks.iter().filter(|c| c.state == CheckState::Pending).count();
        if !pr.checks.is_empty() {
            let verdict = match (failing, pending) {
                (0, 0) => "All passing".to_owned(),
                (0, n) => format!("{n} still running"),
                (f, _) => format!("{f} failing"),
            };
            col = col.child(div().px_3().pb_1().set(Type::Subheadline).text_color(if failing > 0 { p.failed } else { p.secondary }).child(verdict));
        }
        for (ix, c) in pr.checks.iter().enumerate() {
            // only a failure is coloured: it is the one that wants you.
            let (glyph, color, word) = match c.state {
                CheckState::Passing => ("✓", p.secondary, "passing"),
                CheckState::Failing => ("✕", p.failed, "failing"),
                CheckState::Pending => ("○", p.secondary, "running"),
                CheckState::Skipped => ("–", p.tertiary, "skipped"),
                CheckState::Unknown => ("?", p.tertiary, "unknown"),
            };
            let url = c.url.clone();
            col = col.child(
                h_flex()
                    .id(("check", ix))
                    .test_support()
                    .aria_label(SharedString::from(format!("{}, {word}", c.name)))
                    .h(px(24.))
                    .mx_1()
                    .px_2()
                    .gap_2()
                    .rounded(px(theme::R_TILE))
                    .when(url.is_some(), |d| d.hover(|d| d.bg(p.hover)))
                    .on_click(move |_, _, cx| {
                        if let Some(url) = &url {
                            cx.open_url(url);
                        }
                    })
                    .child(div().w(px(12.)).flex_shrink_0().set(Type::Callout).text_color(color).child(glyph))
                    .child(div().flex_1().overflow_hidden().set(Type::Callout).text_color(p.label).truncate().child(c.name.clone()))
                    .child(div().flex_shrink_0().set(Type::Subheadline).text_color(color).child(word)),
            );
        }
        col
    }

    pub(crate) fn artifacts_tab(&self, s: &SessionSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let found = self.model.open.as_ref().map(|o| artifacts(o, &s.cwd)).unwrap_or_default();
        let mut col = v_flex().id("artifacts").size_full().overflow_y_scroll().pb_4();
        if found.is_empty() {
            return col.child(self.quiet("Plans the agent writes and files it creates will collect here."));
        }
        let plans: Vec<_> = found.iter().filter_map(|a| if let Artifact::Plan { id, text } = a { Some((id, text)) } else { None }).collect();
        let files: Vec<_> = found.iter().filter_map(|a| if let Artifact::File { path } = a { Some(path) } else { None }).collect();
        if !plans.is_empty() {
            col = col.child(self.section("Plans"));
        }
        for (id, text) in plans {
            col = col.child(
                div().mx_3().mb_2().p_3().rounded(px(theme::R_CARD)).border_1().border_color(p.separator).set(Type::Callout).text_color(p.label).child(
                    TextView::markdown(ElementId::from(SharedString::from(format!("artifact-plan-{id}"))), text.clone()).selectable(true).style(theme::prose(&p)),
                ),
            );
        }
        if !files.is_empty() {
            col = col.child(self.section("Created Files"));
        }
        for path in files {
            let (name, dir) = split_path(path);
            let open = path.clone();
            col = col.child(
                h_flex()
                    .id(ElementId::from(SharedString::from(format!("artifact-{path}"))))
                    .test_support()
                    .aria_label(SharedString::from(format!("Open {path}")))
                    .h(px(26.))
                    .mx_1()
                    .px_2()
                    .gap_2()
                    .rounded(px(theme::R_TILE))
                    .hover(|d| d.bg(p.hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_file(open.clone(), cx);
                        this.refresh_workspace();
                    }))
                    .child(div().flex_shrink_0().set(Type::Callout).text_color(p.label).child(name.to_owned()))
                    .child(div().flex_1().overflow_hidden().set(Type::Subheadline).text_color(p.tertiary).truncate().child(dir.to_owned())),
            );
        }
        col
    }

    pub(crate) fn info_tab(&self, s: &SessionSummary, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let line = |text: String| div().px_3().set(Type::Callout).text_color(p.label).truncate().child(text);
        let dim = |text: String| div().px_3().set(Type::Subheadline).text_color(p.secondary).truncate().child(text);
        let mut col = v_flex().id("info").size_full().overflow_y_scroll().pb_4();

        col = col.child(self.section("Session")).child(line(project_name(s))).child(dim(s.cwd.clone()));
        if let Some(w) = &s.worktree {
            col = col.child(dim(format!("Branch {}", w.branch)));
        }
        if let Some(model) = &s.model {
            col = col.child(dim(self.model_name(model)));
        }
        col = col.child(dim(format!("${:.3} so far", s.cost_usd)));
        if let (Some(used), Some(window)) = (s.context_used, s.context_window) {
            col = col.child(dim(format!("{} of {} tokens of context used", thousands(used), thousands(window))));
        }

        col = col.child(self.section("Activity"));
        col = match (&s.activity, s.run_state.is_busy()) {
            (Some(a), true) => col.child(line(sentence(a))),
            _ => col.child(dim(state_word(&s.run_state).to_owned())),
        };
        for t in &s.tasks {
            col = col.child(dim(format!("{} {}", if t.background { "In the background:" } else { "Subagent:" }, t.description)));
        }

        col = col.child(
            h_flex().pr_3().items_end().child(self.section("Linked Folders").flex_1()).child(
                Button::new("link-folder").xsmall().ghost().label("Link Folder…").on_click(cx.listener(|this, _, _, cx| this.link_folder(cx))),
            ),
        );
        if s.linked_dirs.is_empty() {
            col = col.child(dim("The agent works only in this session's folder.".into()));
        }
        for (ix, dir) in s.linked_dirs.iter().enumerate() {
            let (sid, path) = (s.id, dir.clone());
            col = col.child(
                h_flex().pr_3().child(div().flex_1().overflow_hidden().child(line(dir.clone()))).child(
                    Button::new(("unlink", ix)).xsmall().ghost().label("Remove").on_click(cx.listener(move |this, _, _, _| this.cmd(Cmd::UnlinkDir { session: sid, path: path.clone() }))),
                ),
            );
        }

        col = col.child(self.section("Tool Servers"));
        if s.mcp.is_empty() {
            col = col.child(dim("None reported yet. They are listed once the agent has started.".into()));
        }
        for m in &s.mcp {
            let (word, color) = match m.status.as_str() {
                "connected" => ("Connected", p.secondary),
                "needs-auth" => ("Needs sign-in", p.blocked),
                "failed" => ("Failed", p.failed),
                "pending" => ("Connecting", p.secondary),
                "disabled" => ("Off", p.tertiary),
                _ => ("Unknown", p.tertiary),
            };
            col = col.child(
                h_flex()
                    .px_3()
                    .gap_2()
                    .child(div().flex_1().overflow_hidden().set(Type::Callout).text_color(p.label).truncate().child(m.name.clone()))
                    .child(div().flex_shrink_0().set(Type::Subheadline).text_color(color).child(word)),
            );
        }

        let sid = s.id;
        let armed = self.confirm_delete == Some(sid);
        col.child(
            div().px_3().pt_6().child(
                Button::new("delete-session")
                    .small()
                    .when(armed, |b| b.danger())
                    .when(!armed, |b| b.outline())
                    .label(if armed { "Delete — Press Again" } else { "Delete Session" })
                    .on_click(cx.listener(move |this, _, _, cx| this.delete(sid, cx))),
            ),
        )
    }
}

/// a commit id is shortened; a branch name is left alone.
fn short_ref(r: &str) -> String {
    if r.len() == 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) { r[..7].to_owned() } else { r.to_owned() }
}

/// `1234567` as `1,234,567`.
pub(crate) fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    // named imports: a wildcard would also bring in the ui library's own `test` attribute.
    use super::{Workspace, relative_to, short_ref, split_path, thousands};
    use photon_proto::DirEntry;

    fn entry(name: &str, is_dir: bool) -> DirEntry {
        DirEntry { name: name.into(), is_dir, size: 0 }
    }

    #[test]
    fn the_tree_shows_open_folders_contents_under_them_and_closed_ones_shut() {
        let mut ws = Workspace::default();
        ws.dirs.insert(String::new(), vec![entry("src", true), entry("docs", true), entry("Cargo.toml", false)]);
        ws.dirs.insert("src".into(), vec![entry("auth", true), entry("main.rs", false)]);
        ws.dirs.insert("src/auth".into(), vec![entry("login.rs", false)]);
        ws.dirs.insert("docs".into(), vec![entry("guide.md", false)]);
        let shape = |ws: &Workspace| ws.tree().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.path)).collect::<Vec<_>>();
        assert_eq!(shape(&ws), ["src", "docs", "Cargo.toml"], "nothing is open to begin with");
        ws.expanded.insert("src".into());
        assert_eq!(shape(&ws), ["src", "  src/auth", "  src/main.rs", "docs", "Cargo.toml"]);
        ws.expanded.insert("src/auth".into());
        assert_eq!(shape(&ws)[2], "    src/auth/login.rs");
        // a folder opened before its listing has arrived just shows nothing yet.
        ws.expanded.insert("vendor".into());
        ws.dirs.get_mut("").unwrap().push(entry("vendor", true));
        assert!(ws.tree().iter().any(|r| r.path == "vendor" && r.open));
    }

    #[test]
    fn paths_are_shown_from_the_sessions_folder_when_they_are_inside_it() {
        assert_eq!(relative_to("/work/api/src/main.rs", "/work/api"), "src/main.rs");
        assert_eq!(relative_to("/work/api/src/main.rs", "/work/api/"), "src/main.rs");
        assert_eq!(relative_to("/work/apiary/x.rs", "/work/api"), "/work/apiary/x.rs", "a folder that merely starts the same is not inside");
        assert_eq!(relative_to("/etc/hosts", "/work/api"), "/etc/hosts");
    }

    #[test]
    fn numbers_and_refs_read_cleanly() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
        assert_eq!(short_ref("0123456789abcdef0123456789abcdef01234567"), "0123456");
        assert_eq!(short_ref("origin/main"), "origin/main");
        assert_eq!(split_path("src/auth/login.rs"), ("login.rs", "src/auth"));
        assert_eq!(split_path("README.md"), ("README.md", ""));
    }
}
