//! the command palette (⌘K): every command, session and theme, by name.
//!
//! it lists exactly what `crate::commands` lists, plus a row per session, so
//! anything reachable by clicking is reachable from here (and from scripts,
//! which run the same entries). items are built only while it is open.

use super::*;
use gpui_kit::component::Icon;
use serde_json::{Value, json};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Scope {
    #[default]
    All,
    Commands,
    Sessions,
    Files,
    Look,
}

const SCOPES: [(Scope, &str); 5] = [(Scope::All, "All"), (Scope::Commands, "Commands"), (Scope::Sessions, "Sessions"), (Scope::Files, "Files"), (Scope::Look, "Look")];

pub(crate) struct PaletteState {
    pub(crate) open: bool,
    pub(crate) input: Entity<InputState>,
    scope: Scope,
    selected: usize,
    focus_pending: bool,
    focus: FocusHandle,
    /// the open session's files, for finding by name; asked for when the palette opens.
    pub(crate) files: std::rc::Rc<Vec<String>>,
    pub(crate) files_for: Option<SessionId>,
}

impl PaletteState {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            open: false,
            input: cx.new(|cx| InputState::new(window, cx).placeholder("Search commands, sessions, files and themes")),
            scope: Scope::All,
            selected: 0,
            focus_pending: false,
            focus: cx.focus_handle(),
            files: Default::default(),
            files_for: None,
        }
    }
}

/// one row: what it says, and the command it runs.
pub(crate) struct Item {
    title: SharedString,
    note: Option<String>,
    icon: &'static str,
    keys: Option<&'static str>,
    cmd: &'static str,
    args: Value,
    scope: Scope,
}

impl Shell {
    pub fn open_palette(&mut self, cx: &mut Context<Self>) {
        // the open session's files: asked for each time, they change.
        if let Some(session) = self.ws_session {
            self.palette_ui.files_for = Some(session);
            self.cmd(Cmd::Ask { tag: "palette.files", req: photon_proto::Request::FsIndex { session } });
        }
        self.palette_ui.open = true;
        self.palette_ui.focus_pending = true;
        self.palette_ui.selected = 0;
        cx.notify();
    }

    /// the query changed: the best match is selected again.
    pub(crate) fn palette_ui_reset(&mut self) {
        self.palette_ui.selected = 0;
    }

    pub fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette_ui.open = false;
        cx.notify();
    }

    /// everything the palette can offer, before searching.
    fn palette_items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = crate::commands::COMMANDS
            .iter()
            // themes come from their files (below), not the three bases.
            .filter(|s| s.palette && s.id != "app.palette" && s.id != "theme.set")
            .map(|s| Item {
                title: s.title.into(),
                note: None,
                icon: s.icon,
                keys: s.keys,
                cmd: s.id,
                args: serde_json::from_str(s.preset).unwrap_or_else(|_| json!({})),
                scope: if s.id.starts_with("theme.") { Scope::Look } else { Scope::Commands },
            })
            .collect();
        for t in self.theme_list() {
            items.push(Item {
                title: format!("Theme: {}", t.name).into(),
                note: (!t.about.is_empty()).then(|| t.about.clone()),
                icon: "palette",
                keys: None,
                cmd: "theme.use",
                args: json!({"name": t.key}),
                scope: Scope::Look,
            });
        }
        // the open session first, then the ones that need you, then the rest.
        let open = self.model.open_summary().map(|s| s.id);
        let mut sessions: Vec<&SessionSummary> = self.model.sessions.iter().collect();
        sessions.sort_by_key(|s| (Some(s.id) != open, s.attention.is_none(), std::cmp::Reverse(s.updated_ms.max(s.created_ms))));
        for s in sessions {
            items.push(Item {
                title: s.goal.clone().into(),
                note: Some(crate::model::project_name(s)),
                icon: if s.worktree.is_some() { "git-branch" } else { "folder" },
                keys: None,
                cmd: "session.open",
                args: json!({"session": s.id.to_string()}),
                scope: Scope::Sessions,
            });
        }
        items
    }

    pub(crate) fn palette_answer(&mut self, result: Result<photon_proto::Response, String>, cx: &mut Context<Self>) {
        if let Ok(photon_proto::Response::Paths { paths, .. }) = result {
            self.palette_ui.files = std::rc::Rc::new(paths);
            cx.notify();
        }
    }

    /// files that match, best first: by name above by path. only the winners
    /// become rows; a big repository costs a scan, not thousands of rows.
    fn file_results(&self, query: &str, limit: usize) -> Vec<(i32, Item)> {
        let mut best: Vec<(i32, &str)> = Vec::new();
        for path in self.palette_ui.files.iter() {
            let name = path.rsplit('/').next().unwrap_or(path);
            let score = if query.is_empty() {
                Some(0)
            } else {
                crate::commands::score(query, name).map(|s| s + 30).or_else(|| crate::commands::score(query, path))
            };
            if let Some(sc) = score {
                best.push((sc - path.len() as i32 / 8, path.as_str()));
            }
        }
        best.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
        best.truncate(limit);
        best.into_iter()
            .map(|(sc, path)| {
                let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
                (sc, Item {
                    title: name.to_owned().into(),
                    note: (!dir.is_empty()).then(|| dir.to_owned()),
                    icon: "file",
                    keys: None,
                    cmd: "files.open",
                    args: json!({"path": path}),
                    scope: Scope::Files,
                })
            })
            .collect()
    }

    /// what the search finds, best first.
    fn palette_results(&self, cx: &App) -> Vec<Item> {
        let query = self.palette_ui.input.read(cx).value().trim().to_string();
        let scope = self.palette_ui.scope;
        let mut found: Vec<(i32, usize, Item)> = self
            .palette_items()
            .into_iter()
            .enumerate()
            .filter(|(_, it)| scope == Scope::All || it.scope == scope)
            .filter_map(|(i, it)| {
                let s = crate::commands::score(&query, &it.title).or_else(|| it.note.as_deref().and_then(|n| crate::commands::score(&query, n)).map(|s| s - 20))?;
                Some((s, i, it))
            })
            .collect();
        // files: in their own scope, and in All once something is typed.
        if scope == Scope::Files || (scope == Scope::All && !query.is_empty()) {
            let base = found.len() + 1_000_000;
            // a file ranks a little under a command that matches as well.
            found.extend(self.file_results(&query, 60).into_iter().enumerate().map(|(i, (sc, it))| (sc - 15, base + i, it)));
        }
        // with nothing typed the order is the natural one; otherwise by score.
        if !query.is_empty() {
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        }
        found.into_iter().take(60).map(|(_, _, it)| it).collect()
    }

    pub(crate) fn palette_move(&mut self, by: i32, cx: &mut Context<Self>) {
        let n = self.palette_results(cx).len();
        if n > 0 {
            self.palette_ui.selected = (self.palette_ui.selected as i32 + by).rem_euclid(n as i32) as usize;
        }
        cx.notify();
    }

    pub(crate) fn palette_scope(&mut self, cx: &mut Context<Self>) {
        let i = SCOPES.iter().position(|(s, _)| *s == self.palette_ui.scope).unwrap_or(0);
        self.palette_ui.scope = SCOPES[(i + 1) % SCOPES.len()].0;
        self.palette_ui.selected = 0;
        cx.notify();
    }

    pub(crate) fn palette_confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut results = self.palette_results(cx);
        let i = self.palette_ui.selected.min(results.len().saturating_sub(1));
        if results.is_empty() {
            return;
        }
        let item = results.swap_remove(i);
        self.palette_ui.open = false;
        self.palette_ui.input.update(cx, |s, cx| s.set_value("", window, cx));
        if let Err(e) = self.exec(item.cmd, &item.args, window, cx) {
            self.error = Some(e);
        }
        cx.notify();
    }

    pub(crate) fn palette_layer(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        if std::mem::take(&mut self.palette_ui.focus_pending) {
            self.palette_ui.input.update(cx, |s, cx| s.focus(window, cx));
        }
        let results = self.palette_results(cx);
        let selected = self.palette_ui.selected.min(results.len().saturating_sub(1));
        // a theme under the selection is tried on; moving off it takes it off.
        let trying = results.get(selected).filter(|it| it.cmd == "theme.use").and_then(|it| it.args.get("name")).and_then(Value::as_str).map(str::to_owned);
        if trying != self.theme_preview && (trying.is_some() || self.theme_preview_by_palette) {
            self.theme_preview_by_palette = trying.is_some();
            self.preview_theme(trying.as_deref(), window, cx);
        }
        let mut list = theme::float_list(&p).id("palette-list").test_support().max_h(px(380.)).overflow_y_scroll();
        if results.is_empty() {
            list = list.child(div().px(theme::z(10.)).py_3().set(Type::Callout).text_color(p.secondary).child("Nothing matches."));
        }
        for (i, it) in results.into_iter().enumerate() {
            let (cmd, args) = (it.cmd, it.args.clone());
            list = list.child(
                theme::float_item_with(&p, SharedString::from(format!("palette-{i}")), Some(it.icon), it.title.clone(), it.note.as_deref(), it.keys, false)
                    .when(i == selected, |d| d.bg(p.hover))
                    .test_support()
                    .aria_label(it.title)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.palette_ui.open = false;
                        if let Err(e) = this.exec(cmd, &args, window, cx) {
                            this.error = Some(e);
                        }
                    })),
            );
        }
        let mut scopes = h_flex().gap_1();
        for (s, name) in SCOPES {
            let on = s == self.palette_ui.scope;
            scopes = scopes.child(
                div()
                    .id(SharedString::from(format!("palette-scope-{name}")))
                    .px(theme::z(8.))
                    .h(theme::z(22.))
                    .flex()
                    .items_center()
                    .rounded(theme::z(6.))
                    .text_size(theme::z(12.))
                    .when(on, |d| d.bg(p.selection).text_color(p.label))
                    .when(!on, |d| d.text_color(p.tertiary).hover(|d| d.text_color(p.label)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.palette_ui.scope = s;
                        this.palette_ui.selected = 0;
                        cx.notify();
                    }))
                    .child(name),
            );
        }
        div()
            .id("palette-layer")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.palette_ui.focus)
            .key_context("Palette")
            .on_action(cx.listener(|this, _: &crate::PaletteClose, _, cx| this.close_palette(cx)))
            .on_action(cx.listener(|this, _: &crate::PaletteUp, _, cx| this.palette_move(-1, cx)))
            .on_action(cx.listener(|this, _: &crate::PaletteDown, _, cx| this.palette_move(1, cx)))
            .on_action(cx.listener(|this, _: &crate::PaletteScope, _, cx| this.palette_scope(cx)))
            .on_click(cx.listener(|this, _, _, cx| this.close_palette(cx)))
            .flex()
            .justify_center()
            .pt(px(72.))
            .child(
                v_flex()
                    .id("palette")
                    .test_support()
                    .aria_label("Command palette")
                    .w(px(560.))
                    .max_w(relative(0.92))
                    .h_auto()
                    .max_h(px(480.))
                    .rounded(theme::z(theme::R_FLOAT))
                    .border_1()
                    .border_color(p.separator)
                    .bg(p.raised)
                    .shadow_lg()
                    .overflow_hidden()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .h(theme::z(44.))
                            .px(theme::z(14.))
                            .gap(theme::z(10.))
                            .child(Icon::empty().path("icons/search.svg").size(theme::z(15.)).text_color(p.tertiary))
                            .child(div().flex_1().text_size(theme::z(14.)).child(Input::new(&self.palette_ui.input).id("palette-input").appearance(false))),
                    )
                    .child(h_flex().px(theme::z(10.)).pb(theme::z(6.)).border_b_1().border_color(p.separator).child(scopes).child(div().flex_1()).child(theme::keycaps(&p, "⇥")))
                    .child(list),
            )
    }
}
