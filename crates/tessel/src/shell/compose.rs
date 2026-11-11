//! drawing the layout from `ui.json`.
//!
//! the tree in `crate::ui` says what goes where; this turns each node into
//! elements and keeps the state the tree implies: which drawers exist, how
//! wide they are and whether they are open, and which options the transcript
//! and panel were given. reloading the file keeps all of it that still applies.

use super::*;
use crate::ui::{Action, Align, Cond, Font, Inset, Justify, Kind, Len, Node, Show, Side, Size, Style, Ui, Variant};
use gpui_kit::component::Icon;
use std::path::Path;
use std::time::SystemTime;

/// a panel at the side that springs open and closed and can be dragged wider.
pub(crate) struct Drawer {
    pub(crate) id: String,
    pub(crate) side: Side,
    pub(crate) open: bool,
    /// how wide it is when open.
    pub(crate) size: f32,
    pub(crate) min: f32,
    pub(crate) max: f32,
    /// the width the file asked for. a new value in the file wins over a dragged one.
    declared: f32,
    /// how wide it is right now.
    pub(crate) w: Spring,
}

/// how the transcript was asked to show things.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct TranscriptOpts {
    /// how wide a row of the conversation is: the reading column.
    pub(crate) width: f32,
    pub(crate) gutter: f32,
    pub(crate) user_right: bool,
    pub(crate) thoughts: Show,
    pub(crate) tools: Show,
}

impl Default for TranscriptOpts {
    fn default() -> Self {
        Self { width: theme::READING_W, gutter: 16.0, user_right: false, thoughts: Show::Shown, tools: Show::Shown }
    }
}

/// read a layout file. no file is the default layout; a broken one is an error to show.
/// what was found is also written beside the reference (`ui/check.txt`), so
/// whoever edited the file, a person or an agent, can read how it went.
pub(crate) fn load_ui(path: &Path) -> (Result<Ui, String>, Option<SystemTime>) {
    let stamp = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let ui = match stamp {
        None => Ok(Ui::default_layout()),
        Some(_) => std::fs::read_to_string(path).map_err(|e| format!("Couldn't read {}: {e}", path.display())).and_then(|t| crate::ui::parse(&t)),
    };
    if stamp.is_some()
        && let Some(dir) = path.parent().map(|d| d.join("ui"))
        && dir.is_dir()
    {
        let _ = std::fs::write(dir.join("check.txt"), crate::ui::harmony::check_text(&ui));
    }
    (ui, stamp)
}

fn eid(s: impl Into<String>) -> ElementId {
    ElementId::from(SharedString::from(s.into()))
}

impl Shell {
    // ---- the layout ----------------------------------------------------------------

    /// take on a layout: drawers it has keep their state, new ones start as
    /// the file says, and gone ones are forgotten.
    pub(crate) fn adopt_ui(&mut self, ui: Ui) {
        let mut next = Vec::new();
        for n in ui.nodes() {
            if n.kind != Kind::Drawer {
                continue;
            }
            let id = n.id.clone().expect("the parser insists drawers have ids");
            let pr = &n.props;
            let min = pr.min.unwrap_or(160.0);
            let max = pr.max.unwrap_or(1200.0).max(min);
            let declared = pr.width.unwrap_or(280.0).clamp(min, max);
            let d = match self.drawers.iter().position(|d| d.id == id) {
                Some(i) => {
                    let mut d = self.drawers.remove(i);
                    d.side = pr.side;
                    d.min = min;
                    d.max = max;
                    d.size = if declared != d.declared { declared } else { d.size.clamp(min, max) };
                    d.declared = declared;
                    if d.open && d.w.target() != d.size {
                        d.w.to(d.size, PANEL);
                    }
                    d
                }
                None => {
                    let open = pr.open.unwrap_or(true);
                    Drawer { id, side: pr.side, open, size: declared, min, max, declared, w: Spring::points(if open { declared } else { 0.0 }) }
                }
            };
            next.push(d);
        }
        self.drawers = next;
        self.panel_drawer = ui.drawer_holding(Kind::Panel);
        self.has_panel = ui.find(Kind::Panel).is_some();
        let conversation = ui.panes().into_iter().find(|p| p.module == Some(crate::ui::Module::Conversation)).map(|p| p.options.clone());
        self.transcript_opts = ui
            .find(Kind::Transcript)
            .map(|t| t.props.clone())
            .or(conversation)
            .map(|t| TranscriptOpts { width: t.width.unwrap_or(theme::READING_W), gutter: t.gutter.unwrap_or(16.0), user_right: t.user_right, thoughts: t.thoughts, tools: t.tools })
            .unwrap_or_default();
        let sessions_pane = ui.panes().into_iter().find(|p| p.module == Some(crate::ui::Module::Sessions)).and_then(|p| p.options.trace);
        self.sessions_trace = ui.find(Kind::Sessions).and_then(|n| n.props.trace).or(sessions_pane).unwrap_or(true);
        // folded panes that are gone from the layout are forgotten.
        let ids: Vec<String> = ui.panes().iter().map(|p| p.id.clone()).collect();
        self.pane_folded.retain(|id, _| ids.contains(id));
        self.panel_tabs = match ui.find(Kind::Panel).and_then(|n| n.props.tabs.clone()) {
            Some(names) => names.iter().filter_map(|n| Tab::ALL.into_iter().find(|t| t.title().eq_ignore_ascii_case(n))).collect(),
            None => Tab::ALL.to_vec(),
        };
        if let Some(first) = self.panel_tabs.first()
            && !self.panel_tabs.contains(&self.ws.tab)
        {
            self.ws.tab = *first;
        }
        self.ui = Rc::new(ui);
    }

    /// take on a layout and redraw with it.
    pub(crate) fn use_ui(&mut self, ui: Ui, window: &mut Window, cx: &mut Context<Self>) {
        let before = self.transcript_opts;
        self.adopt_ui(ui);
        self.palette.set(theme::apply(self.forced_dark, &self.ui.theme, window, cx));
        if self.transcript_opts != before {
            // rows change height when what they show changes.
            let n = self.model.open.as_ref().map_or(0, |o| o.len());
            self.scroller.update(cx, |s, cx| s.remeasure_items(0..n, cx));
        }
        if self.panel_visible() || self.git_visible() {
            self.refresh_workspace();
        }
        cx.notify();
    }

    /// once a second: if the layout file changed, use it.
    pub(crate) fn poll_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.ui_path.clone() else { return };
        let stamp = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        if stamp == self.ui_stamp {
            return;
        }
        let (loaded, stamp) = load_ui(&path);
        self.ui_stamp = stamp;
        match loaded {
            Ok(ui) => {
                self.ui_error = None;
                self.use_ui(ui, window, cx);
            }
            Err(e) => {
                self.ui_error = Some(e);
                cx.notify();
            }
        }
    }

    /// use a layout given as text, without writing it anywhere: tests, benches.
    pub(crate) fn use_ui_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let ui = crate::ui::parse(text)?;
        self.use_ui(ui, window, cx);
        Ok(())
    }

    pub(crate) fn drawer(&self, id: &str) -> Option<&Drawer> {
        self.drawers.iter().find(|d| d.id == id)
    }

    /// how far shut a drawer is: 1 is closed (or missing), 0 is fully open.
    pub(crate) fn drawer_shut(&self, id: &str) -> f32 {
        self.drawer(id).map_or(1.0, |d| 1.0 - (d.w.value() / d.size.max(1.0)).clamp(0.0, 1.0))
    }

    pub fn toggle_drawer(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.drawer(id).is_none() && self.has_pane(id) {
            self.toggle_pane(id, cx);
            return;
        }
        let Some(d) = self.drawers.iter_mut().find(|d| d.id == id) else { return };
        d.open = !d.open;
        let to = if d.open { d.size } else { 0.0 };
        d.w.to(to, PANEL);
        if d.open && self.panel_drawer.as_deref() == Some(id) {
            self.refresh_workspace();
        }
        cx.notify();
    }

    /// whether any of the workspace is on screen, so worth keeping up to date.
    pub(crate) fn panel_visible(&self) -> bool {
        !self.visible_tabs().is_empty()
    }

    /// a session is open, rather than the form for starting one.
    fn viewing_session(&self) -> bool {
        !self.show_new && self.model.open_summary().is_some()
    }

    fn holds(&self, c: &Cond) -> bool {
        match c {
            Cond::Session => self.viewing_session(),
            Cond::Busy => self.viewing_session() && self.model.open_summary().is_some_and(|s| s.run_state.is_busy()),
            Cond::Pending => self.model.open.as_ref().is_some_and(|o| !o.pending.is_empty()),
            Cond::Planning => self.model.open_summary().is_some_and(|s| s.permission_mode.as_deref() == Some("plan")),
            Cond::Attachments => !self.attachments.is_empty(),
            Cond::Connected => matches!(self.model.connection, Connection::Connected),
            Cond::Dark => self.palette.get().dark,
            Cond::Focused => self.window_active,
            Cond::Open(id) => self.drawer(id).is_some_and(|d| d.open) || self.ui.panes().iter().any(|p| &p.id == id && self.pane_open(p)),
            Cond::Not(c) => !self.holds(c),
        }
    }

    pub(crate) fn run(&mut self, action: &Action, window: &mut Window, cx: &mut Context<Self>) {
        match action {
            Action::Toggle(id) => self.toggle_drawer(id, cx),
            Action::NewSession => self.show_new_session(cx),
            Action::Overview => self.toggle_overview(cx),
            Action::Stop => self.stop_agent(cx),
            Action::Send => self.send(window, cx),
            Action::Attach => self.choose_attachments(window, cx),
            Action::Next => self.next_attention(cx),
            Action::Allow => self.allow_pending(cx),
            Action::Focus => self.toggle_focus_mode(cx),
            Action::Customize => self.open_ask(cx),
            Action::Settings => self.open_settings(cx),
        }
    }

    // ---- dividers --------------------------------------------------------------------

    pub(crate) fn start_drag(&mut self, drawer: String, x: f32, cx: &mut Context<Self>) {
        let Some(d) = self.drawer(&drawer) else { return };
        self.drag = Some(Drag { start_w: d.w.value(), drawer, start_x: x, last: (x, Instant::now()), velocity: 0.0 });
        cx.notify();
    }

    pub(crate) fn drag_to(&mut self, x: f32, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.as_mut() else { return };
        // a drawer on the right grows as the pointer moves left.
        let side = self.drawers.iter().find(|d| d.id == drag.drawer).map_or(Side::Left, |d| d.side);
        let toward = if side == Side::Left { 1.0 } else { -1.0 };
        let now = Instant::now();
        let dt = now.saturating_duration_since(drag.last.1).as_secs_f32();
        if dt > 0.001 {
            drag.velocity = toward * (x - drag.last.0) / dt;
            drag.last = (x, now);
        }
        let want = drag.start_w + toward * (x - drag.start_x);
        let id = drag.drawer.clone();
        // it never takes so much that the conversation can't be read.
        let others: f32 = self.drawers.iter().filter(|d| d.id != id).map(|d| d.w.value()).sum();
        let room = (self.viewport.0 - others - theme::CONTENT_MIN).max(0.0);
        let Some(d) = self.drawers.iter_mut().find(|d| d.id == id) else { return };
        let most = if d.side == Side::Right { d.max.min(room).max(d.min) } else { d.max };
        // while your hand is on it, it is exactly where your hand is.
        d.w.snap(want.clamp(0.0, most));
        cx.notify();
    }

    pub(crate) fn drag_end(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        let Some(d) = self.drawers.iter_mut().find(|d| d.id == drag.drawer) else { return };
        let w = d.w.value();
        // where it would coast to, given how fast it was let go.
        let projected = w + drag.velocity * 0.12;
        if projected < d.min * 0.6 {
            // thrown shut.
            d.open = false;
            d.w.fling(0.0, drag.velocity);
        } else {
            d.open = true;
            d.size = w.clamp(d.min, d.max);
            d.w.fling(d.size, drag.velocity);
        }
        cx.notify();
    }

    // ---- drawing ------------------------------------------------------------------------

    /// a node and everything under it, or nothing if its conditions don't hold
    /// or it has nothing to show right now.
    pub(crate) fn node(&self, n: &Node, in_row: bool, now: Millis, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !n.when.iter().all(|c| self.holds(c)) {
            return None;
        }
        // a background the theme didn't plan for gets inks made for it, for
        // this node and everything in it. nothing can be unreadable by accident.
        let outer = self.palette.get();
        let bg = n.style.bg.map(|b| b.resolve(&outer));
        if let Some(bg) = bg.filter(|bg| !self.ui.freestyle && !outer.is_surface(*bg)) {
            self.palette.set(outer.on(bg));
        }
        // the node's own fill is the colour it asked for, not one re-derived for it.
        let parent_bg = self.bg_now.replace(bg);
        let drawn = self.draw(n, in_row, now, cx);
        self.bg_now.set(parent_bg);
        self.palette.set(outer);
        drawn
    }

    fn draw(&self, n: &Node, in_row: bool, now: Millis, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        let s = &n.style;
        Some(match n.kind {
            Kind::Row | Kind::Column => self.container(n, n.kind == Kind::Row, now, cx).into_any_element(),
            Kind::Drawer => return self.drawer_el(n, now, cx),
            Kind::Spacer => self.styled(div().flex_1(), s).into_any_element(),
            Kind::Divider => {
                let line = if in_row { div().w(px(1.)).self_stretch() } else { div().h(px(1.)).w_full() };
                self.styled(line.flex_shrink_0().bg(p.separator), s).into_any_element()
            }
            Kind::Text => self.styled(div().child(n.props.text.clone().unwrap_or_default()), s).into_any_element(),
            Kind::Button => self.button_el(n, cx),
            Kind::Title => {
                let title = match self.model.open_summary().filter(|_| !self.show_new) {
                    Some(s) => s.goal.clone(),
                    None => "New Session".to_owned(),
                };
                let ink = if self.window_active { p.label } else { p.secondary };
                self.styled(div().set(Type::Title).text_color(ink).truncate().child(title), s).into_any_element()
            }
            Kind::Status => {
                let status = self.status_line(now)?;
                self.styled(div().set(Type::Subheadline).text_color(p.secondary).truncate().child(status), s).into_any_element()
            }
            Kind::Connection => {
                let (text, color) = match &self.model.connection {
                    // quiet when all is well.
                    Connection::Connected => return None,
                    Connection::Connecting => ("Connecting to the daemon…".to_owned(), p.secondary),
                    Connection::Down(why) => (format!("Can't reach the daemon. {why}"), p.failed),
                };
                self.styled(div().px_4().py_2().set(Type::Subheadline).text_color(color).child(text), s).into_any_element()
            }
            Kind::Error => {
                let err = self.error.clone()?;
                self.styled(div().px_4().py_1().set(Type::Callout).text_color(p.failed).child(err), s).into_any_element()
            }
            Kind::Sessions => self.styled(self.sessions_list(now, cx), s).into_any_element(),
            Kind::NewSession => self.styled(self.new_session_form(cx), s).into_any_element(),
            Kind::Transcript => self.styled(self.transcript_el(cx), s).into_any_element(),
            Kind::Prompts => self.styled(self.prompts_el(cx)?, s).into_any_element(),
            Kind::Attachments => self.styled(self.attachment_chips(cx)?, s).into_any_element(),
            Kind::Input => self.styled(div().w_full().child(self.input_el(n.props.bare, cx)), s).into_any_element(),
            Kind::ModelPicker => self.styled(div().child(self.model_menu(cx)), s).into_any_element(),
            Kind::ModePicker => self.styled(div().child(self.mode_menu(cx)), s).into_any_element(),
            Kind::Context => self.styled(div().child(self.context_meter_el(cx)?), s).into_any_element(),
            Kind::Panes => {
                // the frame above the panes is title bar: dragging it moves the window.
                let top = s.pt.or(s.py).or(s.p).unwrap_or(0.0);
                let strip = (top > 0.0).then(|| {
                    div()
                        .id("panes-titlebar")
                        .absolute()
                        .top_0()
                        .left(px(16.))
                        .right_0()
                        .h(px(top))
                        .on_double_click(|_, window, _| window.titlebar_double_click())
                        .on_mouse_move(|ev: &MouseMoveEvent, window, _| {
                            if ev.pressed_button == Some(MouseButton::Left) {
                                window.start_window_move();
                            }
                        })
                });
                self.styled(div().relative().child(self.panes_el(n, now, cx)).children(strip), s).into_any_element()
            }
            Kind::Add => self.styled(div().flex_shrink_0().child(self.add_menu("sidebar-add", cx)), s).into_any_element(),
            Kind::Panel => self.styled(div().size_full().child(self.panel(cx)), s).into_any_element(),
        })
    }

    /// what the open session is doing, for under its name.
    pub(crate) fn status_line(&self, now: Millis) -> Option<String> {
        let s = self.model.open_summary().filter(|_| !self.show_new)?;
        let line = match (&s.run_state, &s.activity, s.activity_since_ms) {
            (RunState::Idle, _, _) => "Idle".to_owned(),
            // when it is waiting on you the activity line already says so.
            (RunState::NeedsPermission | RunState::NeedsInput, Some(a), Some(since)) => format!("{} · waiting {}", sentence(a), ago(since, now)),
            (state, Some(a), Some(since)) => format!("{} · {a} · {}", state_word(state), ago(since, now)),
            (state, _, _) => state_word(state).to_owned(),
        };
        (!line.is_empty()).then_some(line)
    }

    fn container(&self, n: &Node, row: bool, now: Millis, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = self.palette.get();
        let mut d = div().id(eid(n.element_id())).flex();
        d = if row { d.flex_row().items_center() } else { d.flex_col() };
        if n.path == "root" {
            d = d.size_full();
        }
        d = self.styled(d, &n.style);
        if let Some(inset) = &n.style.inset {
            let shut = match inset {
                Inset::Always => 1.0,
                Inset::Drawer(id) => self.drawer_shut(id),
            };
            let base = n.style.pl.or(n.style.px).or(n.style.p).unwrap_or(0.0);
            d = d.pl(px(base + theme::WINDOW_BUTTONS_W * shut));
        }
        if n.style.drag {
            d = d.on_double_click(|_, window, _| window.titlebar_double_click()).on_mouse_move(|ev: &MouseMoveEvent, window, _| {
                if ev.pressed_button == Some(MouseButton::Left) {
                    window.start_window_move();
                }
            });
        }
        if n.style.scroll {
            d = d.overflow_y_scroll();
        }
        if let Some(h) = n.style.hover {
            let c = h.resolve(&p);
            d = d.hover(move |st| st.bg(c));
        }
        for c in &n.children {
            if let Some(el) = self.node(c, row, now, cx) {
                d = d.child(el);
            }
        }
        d
    }

    fn drawer_el(&self, n: &Node, now: Millis, cx: &mut Context<Self>) -> Option<AnyElement> {
        let id = n.id.clone()?;
        let d = self.drawer(&id)?;
        let w = d.w.value().max(0.0);
        if w <= 0.5 {
            return None;
        }
        let p = self.palette.get();
        let dragging = self.drag.as_ref().is_some_and(|g| g.drawer == id);
        // a drawer that is closing is clipped, not squeezed: its contents
        // keep their width and slide out of view.
        let mut style = n.style.clone();
        let line = style.border.take().is_none_or(|b| b != crate::ui::Sides::default());
        let mut inner = self.styled(div().id(eid(format!("{id}-content"))).flex().flex_col(), &style).w(px(d.size.max(w))).h_full();
        for c in &n.children {
            if let Some(el) = self.node(c, false, now, cx) {
                inner = inner.child(el);
            }
        }
        let clip = div().w(px(w)).h_full().flex_shrink_0().overflow_hidden().child(inner);
        let grab = id.clone();
        let divider = div()
            .id(eid(format!("{id}-divider")))
            .w(px(1.))
            .h_full()
            .flex_shrink_0()
            .when(line, |d| d.bg(p.separator))
            .child(
                // the line is one point; the part you can grab is wider.
                div()
                    .id(eid(format!("{id}-divider-grip")))
                    .test_support()
                    .aria_label(SharedString::from(format!("Resize {id}")))
                    .absolute()
                    .top_0()
                    // with no line to grab, the handle reaches across the gap to
                    // the edge of what is beside it, which is where the eye looks.
                    .left(px(-3.))
                    .w(px(if line { 7. } else { 14. }))
                    .when(!line && d.side == Side::Right, |d| d.left(px(-11.)))
                    .h_full()
                    .cursor_col_resize()
                    .when(dragging, |d| d.bg(p.quaternary))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, cx| this.start_drag(grab.clone(), f32::from(ev.position.x), cx))),
            );
        let row = div().flex().flex_row().h_full().flex_shrink_0();
        Some(match d.side {
            Side::Left => row.child(clip).child(divider),
            Side::Right => row.child(divider).child(clip),
        }
        .into_any_element())
    }

    fn button_el(&self, n: &Node, cx: &mut Context<Self>) -> AnyElement {
        let pr = &n.props;
        let action = pr.action.clone().expect("the parser insists buttons have actions");
        let mut b = Button::new(eid(n.element_id()));
        b = match pr.variant {
            Variant::Ghost => b.ghost(),
            Variant::Primary => b.primary(),
            Variant::Outline => b.outline(),
            Variant::Text => b.text(),
        };
        b = match pr.size {
            Size::XSmall => b.xsmall(),
            Size::Small => b.small(),
            Size::Medium => b,
        };
        if let Some(icon) = &pr.icon {
            b = b.icon(Icon::empty().path(format!("icons/{icon}.svg")));
        }
        if let Some(label) = &pr.label {
            b = b.label(label.clone());
        }
        let tooltip = match (&pr.tooltip, &action) {
            (Some(t), _) => t.clone(),
            (None, Action::Toggle(id)) => {
                let open = self.drawer(id).is_some_and(|d| d.open);
                format!("{} {}", if open { "Hide" } else { "Show" }, sentence(id))
            }
            (None, a) => a.tooltip(),
        };
        let b = b.tooltip(tooltip).on_click(cx.listener(move |this, _, window, cx| this.run(&action, window, cx)));
        self.styled(div().flex_shrink_0().child(b), &n.style).into_any_element()
    }

    pub(crate) fn transcript_el(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let p = self.palette.get();
        let this = cx.entity();
        // the list's default puts 2rem under every row, which suits chat
        // bubbles and not a dense log of tool calls. rows space themselves.
        let list = MessageScroller::new("transcript", self.scroller.clone(), move |ix, _window, cx| this.read(cx).row(ix, &this, cx))
            .with_row_style(StyleRefinement::default().pb_0().px_0());
        let truncated = self.model.open.as_ref().is_some_and(|o| o.has_more);
        div()
            .id("transcript-pane")
            .flex()
            .flex_col()
            .min_h_0()
            .when(truncated, |d| {
                d.child(h_flex().justify_center().py_1().set(Type::Subheadline).text_color(p.secondary).child("Showing the most recent part of this session."))
            })
            .child(div().flex_1().min_h_0().child(list.size_full()))
    }

    pub(crate) fn prompts_el(&self, cx: &mut Context<Self>) -> Option<Div> {
        // the prompt bar keeps drawing what it last showed while it closes.
        let pending: Vec<PermissionRequest> = match self.model.open.as_ref().map(|o| &o.pending).filter(|pending| !pending.is_empty()) {
            Some(live) => live.clone(),
            None => self.prompt_ghost.clone(),
        };
        let reveal = self.prompt_open.value().clamp(0.0, 1.0);
        if reveal <= 0.0 || pending.is_empty() {
            return None;
        }
        let cards = self.prompt_cards(&pending, cx);
        Some(div().w_full().opacity(reveal).child(MotionReveal::new("prompt-bar", reveal, cards)))
    }

    /// apply a node's style keys over a component's own defaults.
    pub(crate) fn styled<E: Styled>(&self, mut e: E, s: &Style) -> E {
        let p = &self.palette.get();
        let len = |e: E, l: Len, w: bool| match (l, w) {
            (Len::Px(v), true) => e.w(px(v)),
            (Len::Full, true) => e.w_full(),
            (Len::Auto, true) => e.w_auto(),
            (Len::Px(v), false) => e.h(px(v)),
            (Len::Full, false) => e.h_full(),
            (Len::Auto, false) => e.h_auto(),
        };
        if let Some(w) = s.w {
            e = len(e, w, true);
        }
        if let Some(h) = s.h {
            e = len(e, h, false);
        }
        if let Some(v) = s.min_w {
            e = e.min_w(px(v));
        }
        if let Some(v) = s.max_w {
            e = e.max_w(px(v));
        }
        if let Some(v) = s.min_h {
            e = e.min_h(px(v));
        }
        if let Some(v) = s.max_h {
            e = e.max_h(px(v));
        }
        if s.grow {
            e = e.flex_1();
        }
        match s.shrink {
            Some(false) => e = e.flex_shrink_0(),
            Some(true) => e = e.flex_shrink(1.0),
            None => {}
        }
        if let Some(v) = s.p {
            e = e.p(px(v));
        }
        if let Some(v) = s.px {
            e = e.px(px(v));
        }
        if let Some(v) = s.py {
            e = e.py(px(v));
        }
        for (v, side) in [(s.pt, 0), (s.pr, 1), (s.pb, 2), (s.pl, 3)] {
            if let Some(v) = v {
                e = match side {
                    0 => e.pt(px(v)),
                    1 => e.pr(px(v)),
                    2 => e.pb(px(v)),
                    _ => e.pl(px(v)),
                };
            }
        }
        if let Some(v) = s.gap {
            e = e.gap(px(v));
        }
        e = match s.align {
            Some(Align::Start) => e.items_start(),
            Some(Align::Center) => e.items_center(),
            Some(Align::End) => e.items_end(),
            Some(Align::Stretch) => e.items_stretch(),
            None => e,
        };
        e = match s.justify {
            Some(Justify::Start) => e.justify_start(),
            Some(Justify::Center) => e.justify_center(),
            Some(Justify::End) => e.justify_end(),
            Some(Justify::Between) => e.justify_between(),
            None => e,
        };
        if let Some(c) = s.bg {
            e = e.bg(self.bg_now.get().unwrap_or_else(|| c.resolve(p)));
        }
        if let Some(c) = s.color {
            // asked-for text colours still have to read on what they sit on.
            let c = c.resolve(p);
            e = e.text_color(if self.ui.freestyle { c } else { theme::readable(c, p.window, 4.5) });
        }
        if let Some(b) = s.border {
            if b.top {
                e = e.border_t_1();
            }
            if b.right {
                e = e.border_r_1();
            }
            if b.bottom {
                e = e.border_b_1();
            }
            if b.left {
                e = e.border_l_1();
            }
            e = e.border_color(s.border_color.map_or(p.separator, |c| c.resolve(p)));
        }
        if let Some(r) = s.radius {
            e = e.rounded(px(r));
        }
        match s.font {
            Some(Font::Style(t)) => e = e.set(t),
            Some(Font::Size(size)) => e = e.text_size(px(size)).line_height(px((size * 1.4).round())),
            None => {}
        }
        if let Some(w) = s.weight {
            e = e.font_weight(w);
        }
        if let Some(o) = s.opacity {
            e = e.opacity(o.clamp(0.0, 1.0));
        }
        if s.clip {
            e = e.overflow_hidden();
        }
        if s.shadow {
            e = e.shadow_md();
        }
        if s.wrap {
            e = e.flex_wrap();
        }
        e
    }
}
