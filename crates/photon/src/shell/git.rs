//! the git module: history as a graph, and the next commit being put together.
//!
//! the top is the work tree: what's staged and what isn't, a file at a time
//! (or, from the diff, a line at a time), a message (or one written for you),
//! commit, and undo for anything photon did. below is every branch's history
//! as lanes, virtualized, a page at a time; pick a commit for its files and
//! diff. it all goes through the daemon: nothing here runs git.

use super::*;
use crate::graph::{self, Level};
use crate::theme::{ButtonKind, z};
use gpui_kit::component::Icon;
use gpui_kit::component::diff::{Diff, DiffFile, DiffLineRange, DiffMode, DiffSide, DiffState};
use crate::ui::Module;
use photon_proto::ChangeStatus;
use gpui_kit::{PathBuilder, UniformListScrollHandle, canvas, fill, point, uniform_list};
use photon_proto::{Commit, FileChange, Request, Response, WorkTree};
use std::rc::Rc;

/// a row of the history list.
const ROW_H: f32 = 28.0;
/// one lane of the graph.
const LANE_W: f32 = 12.0;
const PAGE: u32 = 200;

pub(crate) struct GitView {
    session: Option<SessionId>,
    commits: Vec<Commit>,
    rows: Rc<Vec<(Commit, graph::Row)>>,
    more: bool,
    loading: bool,
    tree: WorkTree,
    selected: Option<String>,
    /// a file of the work tree being looked at (to stage lines of it), and whether its staged side.
    wt: Option<(String, bool)>,
    files: Vec<FileChange>,
    file: Option<String>,
    diff: Option<Entity<DiffState>>,
    pub(crate) message: Entity<InputState>,
    suggesting: bool,
    error: Option<String>,
    list: UniformListScrollHandle,
}

impl GitView {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Shell>) -> Self {
        Self {
            session: None,
            commits: Vec::new(),
            rows: Rc::default(),
            more: false,
            loading: false,
            tree: WorkTree::default(),
            selected: None,
            wt: None,
            files: Vec::new(),
            file: None,
            diff: None,
            message: cx.new(|cx| InputState::new(window, cx).placeholder("Commit message")),
            suggesting: false,
            error: None,
            list: UniformListScrollHandle::new(),
        }
    }
}

/// a lane's colour: a few distinct hues at one weight, readable on either appearance.
fn lane_color(i: u16, dark: bool) -> Hsla {
    const HUES: [f32; 7] = [0.60, 0.36, 0.08, 0.83, 0.50, 0.0, 0.15];
    gpui_kit::hsla(HUES[i as usize % HUES.len()], 0.62, if dark { 0.62 } else { 0.46 }, 1.0)
}

/// "HEAD -> main" is main, and where you are.
fn ref_label(r: &str) -> (String, bool) {
    match r.strip_prefix("HEAD -> ") {
        Some(b) => (b.to_owned(), true),
        None => (r.strip_prefix("tag: ").map_or_else(|| r.to_owned(), |t| format!("⌁ {t}")), r == "HEAD"),
    }
}

fn ago(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..60 => "now".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        86_400..2_592_000 => format!("{}d", s / 86_400),
        _ => format!("{}mo", s / 2_592_000),
    }
}

impl Shell {
    fn git_ask(&mut self, tag: &'static str, req: Request) {
        self.cmd(Cmd::Ask { tag, req });
    }

    pub(crate) fn git_visible(&self) -> bool {
        self.ui.panes().into_iter().any(|p| p.module == Some(Module::Git) && self.pane_open(p))
    }

    /// bring the git pane up to date, if one is on screen.
    pub(crate) fn refresh_git(&mut self) {
        if !self.git_visible() {
            return;
        }
        let Some(session) = self.ws_session else { return };
        if self.git.session != Some(session) {
            // another session: another repository, start clean.
            self.git.session = Some(session);
            self.git.commits.clear();
            self.git.rows = Rc::default();
            self.git.tree = WorkTree::default();
            self.git.selected = None;
            self.git.files.clear();
            self.git.file = None;
            self.git.diff = None;
            self.git.error = None;
        }
        self.git_ask("git.tree", Request::GitWorkTree { session });
        self.git.loading = true;
        self.git_ask("git.log", Request::GitLog { session, skip: 0, limit: PAGE.max(self.git.commits.len() as u32) });
        if let Some(sha) = self.git.selected.clone() {
            self.git_ask("git.show", Request::GitShow { session, sha });
        }
    }

    pub(crate) fn git_answer(&mut self, tag: &'static str, result: Result<Response, String>, cx: &mut Context<Self>) {
        let r = match result {
            Ok(r) => r,
            Err(e) => {
                self.git.suggesting = false;
                self.git.loading = false;
                self.git.error = Some(e);
                cx.notify();
                return;
            }
        };
        match (tag, r) {
            ("git.log", Response::GitLog { log }) => {
                self.git.loading = false;
                self.git.more = log.more;
                self.git.commits = log.commits;
                self.relayout_git();
            }
            ("git.more", Response::GitLog { log }) => {
                self.git.loading = false;
                self.git.more = log.more;
                self.git.commits.extend(log.commits);
                self.relayout_git();
            }
            (_, Response::WorkTree { tree }) => {
                let changed = tag != "git.tree";
                self.git.tree = tree;
                self.git.error = None;
                // the file being staged by lines: show what's left (or close it when nothing is).
                if let Some((path, staged)) = self.git.wt.clone() {
                    let list = if staged { &self.git.tree.staged } else { &self.git.tree.unstaged };
                    if list.iter().any(|f| f.path == path) {
                        self.git_open_wt(path, staged);
                    } else {
                        self.git.wt = None;
                        self.git.diff = None;
                    }
                }
                // an undo can take a commit back: the history moves too.
                if changed {
                    self.refresh_workspace();
                }
            }
            ("git.show", Response::Changed { files }) => {
                let keep = self.git.file.clone().filter(|f| files.iter().any(|c| &c.path == f));
                self.git.files = files;
                match keep.or_else(|| self.git.files.first().map(|f| f.path.clone())) {
                    Some(path) => self.git_pick_file(path),
                    None => self.git.diff = None,
                }
            }
            ("git.diff" | "git.wtdiff", Response::Diff { diff }) => {
                let files = if diff.binary { Vec::new() } else { DiffFile::parse(&diff.unified).unwrap_or_default() };
                if files.is_empty() {
                    self.git.diff = None;
                } else {
                    match &self.git.diff {
                        Some(state) => state.update(cx, |s, cx| s.set_files(files, cx)),
                        None => self.git.diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Unified))),
                    }
                }
            }
            ("git.commit", Response::Committed { .. }) => {
                self.git.error = None;
                self.git_clear_message = true;
                self.refresh_workspace();
            }
            ("git.suggest", Response::Suggested { message }) => {
                self.git.suggesting = false;
                self.git_suggestion = Some(message);
            }
            _ => {}
        }
        cx.notify();
    }

    fn relayout_git(&mut self) {
        let rows = graph::layout(&self.git.commits);
        self.git.rows = Rc::new(self.git.commits.iter().cloned().zip(rows).collect());
    }

    fn git_pick_file(&mut self, path: String) {
        let (Some(session), Some(sha)) = (self.git.session, self.git.selected.clone()) else { return };
        self.git.file = Some(path.clone());
        self.git_ask("git.diff", Request::GitDiff { session, path, at: Some(sha), index: None });
    }

    /// look at a work tree file's diff, to stage (or unstage) lines of it.
    fn git_open_wt(&mut self, path: String, staged: bool) {
        let Some(session) = self.git.session else { return };
        self.git.selected = None;
        self.git.files.clear();
        self.git.wt = Some((path.clone(), staged));
        self.git_ask("git.wtdiff", Request::GitDiff { session, path, at: None, index: Some(staged) });
    }

    #[cfg(test)]
    pub(crate) fn git_pick_lines_for_test(&mut self, start: usize, end: usize, cx: &mut Context<Self>) {
        let path = self.git.wt.clone().map(|(p, _)| p).unwrap_or_default();
        self.git_stage_range(&DiffLineRange::new(path, DiffSide::Modified, start, end), cx);
    }

    /// lines picked in a work tree diff: stage them (or, in the staged diff, unstage them).
    fn git_stage_range(&mut self, range: &DiffLineRange, cx: &mut Context<Self>) {
        let (Some(session), Some((path, staged))) = (self.git.session, self.git.wt.clone()) else { return };
        let (a, b) = (range.start().min(range.end()), range.start().max(range.end()));
        let old = range.side() == DiffSide::Original;
        self.git_op(Request::GitStageLines { session, path, start: a as u32, end: b as u32, unstage: staged, old });
        cx.notify();
    }

    pub(crate) fn git_select(&mut self, sha: String, cx: &mut Context<Self>) {
        self.git.wt = None;
        let Some(session) = self.git.session else { return };
        if self.git.selected.as_deref() == Some(sha.as_str()) {
            self.git.selected = None;
            self.git.files.clear();
            self.git.diff = None;
        } else {
            self.git.selected = Some(sha.clone());
            self.git.file = None;
            self.git_ask("git.show", Request::GitShow { session, sha });
        }
        cx.notify();
    }

    fn git_op(&mut self, req: Request) {
        self.git.error = None;
        self.git_ask("git.op", req);
    }

    fn git_commit(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.git.session else { return };
        let message = self.git.message.read(cx).value().trim().to_string();
        if message.is_empty() {
            self.git.error = Some("Write a message first (or ask for one).".into());
            cx.notify();
            return;
        }
        self.git_ask("git.commit", Request::GitCommit { session, message });
    }

    /// apply what arrived for the message box (it needs a window to change).
    pub(crate) fn sync_git_message(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.git_clear_message) {
            self.git.message.update(cx, |s, cx| s.set_value("", window, cx));
        }
        if let Some(m) = self.git_suggestion.take() {
            self.git.message.update(cx, |s, cx| s.set_value(m, window, cx));
        }
    }

    fn git_file_row(&self, f: &FileChange, staged: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let (letter, tint) = match f.status {
            ChangeStatus::Added => ("A", p.finished),
            ChangeStatus::Untracked => ("U", p.finished),
            ChangeStatus::Modified => ("M", p.blocked),
            ChangeStatus::Deleted => ("D", p.failed),
            ChangeStatus::Unknown => ("?", p.tertiary),
        };
        let (dir, name) = match f.path.rsplit_once('/') {
            Some((d, n)) => (format!("{d}/"), n.to_owned()),
            None => (String::new(), f.path.clone()),
        };
        let path = f.path.clone();
        let open = f.path.clone();
        let on = self.git.wt.as_ref().is_some_and(|(p0, s0)| *p0 == f.path && *s0 == staged);
        let id = SharedString::from(format!("git-{}-{}", if staged { "staged" } else { "unstaged" }, f.path));
        h_flex()
            .id(id.clone())
            .test_support()
            .group(id.clone())
            .h(z(26.))
            .px(z(8.))
            .gap(z(8.))
            .rounded(z(theme::R_TILE))
            .cursor_pointer()
            .when(on, |d| d.bg(p.selection))
            .when(!on, |d| d.hover(|d| d.bg(p.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.git_open_wt(open.clone(), staged);
                cx.notify();
            }))
            .child(div().w(z(12.)).flex_shrink_0().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(tint).child(letter))
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .set(Type::Code)
                    .child(div().min_w_0().flex_shrink(1.0).truncate().text_color(p.tertiary).child(dir))
                    .child(div().flex_shrink_0().text_color(p.label).child(name)),
            )
            .when(f.added + f.removed > 0, |d| {
                d.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap_1()
                        .set(Type::Subheadline)
                        .when(f.added > 0, |d| d.child(div().text_color(p.finished).child(format!("+{}", f.added))))
                        .when(f.removed > 0, |d| d.child(div().text_color(p.failed).child(format!("−{}", f.removed)))),
                )
            })
            .child(
                div().invisible().group_hover(id.clone(), |s| s.visible()).child(
                    theme::button(&p, SharedString::from(format!("{id}-act")), "", Some(if staged { "minus" } else { "plus" }), ButtonKind::Ghost)
                        .h(z(22.))
                        .px(z(6.))
                        .test_support()
                        .aria_label(SharedString::from(format!("{} {}", if staged { "Unstage" } else { "Stage" }, f.path)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            let Some(session) = this.git.session else { return };
                            let paths = vec![path.clone()];
                            this.git_op(if staged { Request::GitUnstage { session, paths } } else { Request::GitStage { session, paths } });
                            cx.notify();
                        })),
                ),
            )
        .into_any_element()
    }

    fn git_section(&self, title: &'static str, files: &[FileChange], staged: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let all: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        v_flex()
            .gap(px(1.))
            .child(
                h_flex()
                    .h(z(24.))
                    .px(z(8.))
                    .gap_2()
                    .child(div().set(Type::Subheadline).font_weight(FontWeight::MEDIUM).text_color(p.secondary).child(title))
                    .child(div().set(Type::Subheadline).text_color(p.tertiary).child(files.len().to_string()))
                    .child(div().flex_1())
                    .when(!files.is_empty(), |d| {
                        d.child(
                            div()
                                .id(SharedString::from(format!("git-{title}-all")))
                                .set(Type::Subheadline)
                                .text_color(p.tertiary)
                                .cursor_pointer()
                                .hover(|d| d.text_color(p.label))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(session) = this.git.session else { return };
                                    let paths = all.clone();
                                    this.git_op(if staged { Request::GitUnstage { session, paths } } else { Request::GitStage { session, paths } });
                                    cx.notify();
                                }))
                                .child(if staged { "Unstage all" } else { "Stage all" }),
                        )
                    }),
            )
            .children({
                let mut rows = Vec::with_capacity(files.len());
                for f in files {
                    rows.push(self.git_file_row(f, staged, cx));
                }
                rows
            })
        .into_any_element()
    }

    fn git_tree_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let t = &self.git.tree;
        let undo = t.undo.clone();
        v_flex()
            .id("git-tree")
            .flex_shrink_0()
            .max_h(relative(0.5))
            .overflow_y_scroll()
            .p(z(8.))
            .gap(z(6.))
            .child(self.git_section("Staged", &t.staged.clone(), true, cx))
            .child(self.git_section("Changes", &t.unstaged.clone(), false, cx))
            .child(
                v_flex()
                    .gap(z(6.))
                    .px(z(4.))
                    .child(div().rounded(z(theme::R_CONTROL)).border_1().border_color(p.separator).bg(p.raised).px(z(4.)).child(Input::new(&self.git.message).id("git-message").appearance(false)))
                    .child(
                        h_flex()
                            .gap(z(6.))
                            .children(undo.map(|what| {
                                theme::button(&p, "git-undo", format!("Undo {}", what.to_lowercase()), Some("undo-2"), ButtonKind::Ghost).text_color(p.secondary).on_click(cx.listener(|this, _, _, cx| {
                                    if let Some(session) = this.git.session {
                                        this.git_op(Request::GitUndo { session });
                                    }
                                    cx.notify();
                                }))
                            }))
                            .child(div().flex_1())
                            .child(
                                theme::button(&p, "git-suggest", if self.git.suggesting { "Writing…" } else { "Suggest" }, Some("sparkles"), ButtonKind::Secondary)
                                    .when(t.staged.is_empty() || self.git.suggesting, |d| d.opacity(0.5))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        if this.git.suggesting || this.git.tree.staged.is_empty() {
                                            return;
                                        }
                                        if let Some(session) = this.git.session {
                                            this.git.suggesting = true;
                                            this.git_ask("git.suggest", Request::GitSuggestMessage { session });
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child(
                                theme::button(&p, "git-commit", "Commit", Some("git-commit-horizontal"), ButtonKind::Primary)
                                    .when(t.staged.is_empty(), |d| d.opacity(0.5))
                                    .test_support()
                                    .aria_label("Commit")
                                    .on_click(cx.listener(|this, _, _, cx| this.git_commit(cx))),
                            ),
                    )
                    .children(self.git.error.clone().map(|e| div().set(Type::Callout).text_color(p.failed).child(e))),
            )
        .into_any_element()
    }

    fn git_history_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        if self.git.rows.is_empty() {
            return div().p_4().set(Type::Callout).text_color(p.secondary).child(if self.git.loading { "Reading history…" } else { "No commits yet." }).into_any_element();
        }
        let rows = self.git.rows.clone();
        let lanes = rows.iter().map(|(_, r)| r.width).max().unwrap_or(1).min(12);
        let gutter = LANE_W * f32::from(lanes) + 8.0;
        let selected = self.git.selected.clone();
        let me = cx.entity().downgrade();
        let more = self.git.more;
        let count = rows.len() + usize::from(more);
        let dark = p.dark;
        uniform_list("git-history", count, move |range, _, _| {
            range
                .map(|i| {
                    if i >= rows.len() {
                        let me = me.clone();
                        return h_flex()
                            .id("git-more")
                            .h(px(ROW_H))
                            .px_3()
                            .set(Type::Callout)
                            .text_color(p.secondary)
                            .cursor_pointer()
                            .hover(|d| d.text_color(p.label))
                            .on_click(move |_, _, cx| {
                                let _ = me.update(cx, |this, cx| this.git_load_more(cx));
                            })
                            .child("Older commits…")
                            .into_any_element();
                    }
                    let (c, row) = &rows[i];
                    let row = row.clone();
                    let sha = c.sha.clone();
                    let on = selected.as_deref() == Some(c.sha.as_str());
                    let head = c.refs.iter().any(|r| r.starts_with("HEAD"));
                    let graph = canvas(
                        |_, _, _| (),
                        move |b, (), window, _| {
                            let x = |lane: u16| b.origin.x + px(4. + LANE_W * f32::from(lane) + LANE_W / 2.);
                            let y = |l: Level| match l {
                                Level::Top => b.origin.y,
                                Level::Middle => b.origin.y + b.size.height / 2.,
                                Level::Bottom => b.origin.y + b.size.height,
                            };
                            for e in &row.edges {
                                if e.from.0 >= lanes || e.to.0 >= lanes {
                                    continue;
                                }
                                let (a, z2) = (point(x(e.from.0), y(e.from.1)), point(x(e.to.0), y(e.to.1)));
                                let mut path = PathBuilder::stroke(px(1.5));
                                path.move_to(a);
                                if a.x == z2.x {
                                    path.line_to(z2);
                                } else {
                                    let mid = (a.y + z2.y) / 2.;
                                    path.cubic_bezier_to(z2, point(a.x, mid), point(z2.x, mid));
                                }
                                if let Ok(path) = path.build() {
                                    window.paint_path(path, lane_color(e.color, dark));
                                }
                            }
                            let r = if head { 4.5 } else { 3.5 };
                            let c = point(x(row.lane.min(lanes - 1)), y(Level::Middle));
                            let dot = gpui_kit::Bounds::new(point(c.x - px(r), c.y - px(r)), gpui_kit::size(px(2. * r), px(2. * r)));
                            let color = lane_color(row.color, dark);
                            if head {
                                window.paint_quad(fill(dot, p.window).corner_radii(px(r)).border_widths(px(2.)).border_color(color));
                            } else {
                                window.paint_quad(fill(dot, color).corner_radii(px(r)));
                            }
                        },
                    )
                    .w(px(gutter))
                    .h_full()
                    .flex_shrink_0();
                    let me = me.clone();
                    h_flex()
                        .id(SharedString::from(format!("commit-{}", c.sha)))
                        .test_support()
                        .w_full()
                        .h(px(ROW_H))
                        .pr_2()
                        .gap_2()
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                        .on_click(move |_, _, cx| {
                            let sha = sha.clone();
                            let _ = me.update(cx, |this, cx| this.git_select(sha, cx));
                        })
                        .child(graph)
                        .children(c.refs.iter().take(3).map(|r| {
                            let (label, here) = ref_label(r);
                            theme::badge(label, if here { p.accent } else { p.secondary })
                        }))
                        .child(div().flex_1().min_w_0().truncate().set(Type::Body).text_color(p.label).child(c.subject.clone()))
                        .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(format!("{} · {}", c.author.split_whitespace().next().unwrap_or(""), ago(c.time_ms))))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.git.list)
        .flex_1()
        .min_h_0()
        .into_any_element()
    }

    pub(crate) fn git_load_more(&mut self, cx: &mut Context<Self>) {
        let Some(session) = self.git.session else { return };
        if self.git.loading {
            return;
        }
        self.git.loading = true;
        self.git_ask("git.more", Request::GitLog { session, skip: self.git.commits.len() as u32, limit: PAGE });
        cx.notify();
    }

    fn git_wt_detail_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (path, staged) = self.git.wt.clone()?;
        let p = self.palette.get();
        let me = cx.entity().downgrade();
        Some(
            v_flex()
                .id("git-wt-detail")
                .test_support()
                .h(relative(0.45))
                .flex_shrink_0()
                .border_t_1()
                .border_color(p.separator)
                .child(
                    h_flex()
                        .h(z(30.))
                        .px_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(p.separator)
                        .child(div().flex_1().min_w_0().truncate().set(Type::Code).child(path))
                        .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(if staged {
                            "Staged · pick lines with + to unstage them"
                        } else {
                            "Pick lines with + to stage just those"
                        }))
                        .child(
                            div()
                                .id("git-wt-close")
                                .flex_shrink_0()
                                .cursor_pointer()
                                .text_color(p.tertiary)
                                .hover(|d| d.text_color(p.label))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.git.wt = None;
                                    this.git.diff = None;
                                    cx.notify();
                                }))
                                .child(Icon::empty().path("icons/x.svg").size(z(13.))),
                        ),
                )
                .child(div().flex_1().min_h_0().overflow_hidden().children(self.git.diff.as_ref().map(|d| {
                    let me = me.clone();
                    Diff::new(d).size_full().border_0().on_add_annotation(move |range, _, cx| {
                        let _ = me.update(cx, |this, cx| this.git_stage_range(range, cx));
                    })
                })))
                .into_any_element(),
        )
    }

    fn git_detail_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.git.wt.is_some() {
            return self.git_wt_detail_el(cx);
        }
        let sha = self.git.selected.clone()?;
        let p = self.palette.get();
        let commit = self.git.commits.iter().find(|c| c.sha == sha)?;
        let mut files = v_flex().id("git-files").max_h(z(120.)).overflow_y_scroll().py_1();
        for f in &self.git.files {
            let on = self.git.file.as_deref() == Some(f.path.as_str());
            let path = f.path.clone();
            files = files.child(
                h_flex()
                    .id(SharedString::from(format!("git-file-{}", f.path)))
                    .h(z(24.))
                    .px_3()
                    .gap_2()
                    .set(Type::Code)
                    .when(on, |d| d.bg(p.selection))
                    .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.git_pick_file(path.clone());
                        cx.notify();
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(f.path.clone()))
                    .child(div().set(Type::Subheadline).text_color(p.finished).child(format!("+{}", f.added)))
                    .child(div().set(Type::Subheadline).text_color(p.failed).child(format!("−{}", f.removed))),
            );
        }
        Some(
            v_flex()
                .h(relative(0.45))
                .flex_shrink_0()
                .border_t_1()
                .border_color(p.separator)
                .child(
                    h_flex()
                        .h(z(30.))
                        .px_3()
                        .gap_2()
                        .border_b_1()
                        .border_color(p.separator)
                        .child(div().set(Type::Code).text_color(p.tertiary).child(sha[..sha.len().min(8)].to_owned()))
                        .child(div().flex_1().min_w_0().truncate().set(Type::Callout).font_weight(FontWeight::MEDIUM).child(commit.subject.clone())),
                )
                .child(files)
                .child(div().flex_1().min_h_0().border_t_1().border_color(p.separator).overflow_hidden().children(self.git.diff.as_ref().map(|d| Diff::new(d).size_full().border_0())))
                .into_any_element(),
        )
    }

    /// the git pane.
    pub(crate) fn git_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        if self.git.session.is_some() && !self.git.tree.is_repo && !self.git.loading && self.git.rows.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .child(Icon::empty().path("icons/git-graph.svg").size(z(20.)).text_color(p.tertiary))
                .child(div().set(Type::Callout).text_color(p.secondary).child("This folder isn't a git repository."))
                .into_any_element();
        }
        v_flex()
            .size_full()
            .child(self.git_tree_el(cx))
            .child(div().h(px(1.)).flex_shrink_0().bg(p.separator))
            .child(self.git_history_el(cx))
            .children(self.git_detail_el(cx))
            .into_any_element()
    }
}
