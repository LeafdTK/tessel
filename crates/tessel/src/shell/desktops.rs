//! desktops: several layouts, each with what was on it.
//!
//! a desktop is a view, not a container. it has a layout of its own, the
//! projects its sidebar lists, and whatever chats its panes held; sessions
//! belong to the daemon and run whichever desktop is showing. leaving a
//! desktop keeps its chats followed and its scroll where it was, so coming
//! back is instant and nothing is fetched twice.
//!
//! in `ui.json` they are a `desktops` list and a `desktop` key saying which is
//! showing (see `ui::desktop_overlay`). switching writes that key, and the file
//! reloading is what changes the window, so a desktop changed by hand, by a
//! script or by an agent all go the same way.

use super::chats::ChatView;
use super::Shell;
use crate::motion::Spring;
use crate::theme::Typeset as _;
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::*;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tessel_proto::SessionId;

/// what a desktop had on it when you left.
pub(crate) struct Stashed {
    pub(crate) open: Option<SessionId>,
    pub(crate) chat_pane: Option<String>,
    pub(crate) views: HashMap<String, ChatView>,
    pub(crate) scroller: Entity<MessageScrollerState>,
    pub(crate) pane_focus: Option<String>,
    pub(crate) show_new: bool,
    /// its chat was changed while it was away: start at that chat's end.
    pub(crate) fresh: bool,
}

/// a desktop being swiped to: the panes follow the fingers, the next desktop
/// (its panes' outlines) comes in beside them, and letting go either carries
/// on to it or springs back, as spaces do on the mac.
pub(crate) struct DeskDrag {
    /// how far the panes are moved sideways, in points. 0: at rest.
    pub(crate) x: Spring,
    /// the same, for the panes' own measuring, which must not see the move.
    pub(crate) shift: std::rc::Rc<std::cell::Cell<f32>>,
    /// the desktop coming in beside, and its panes.
    pub(crate) peek: Option<(usize, Option<crate::ui::PaneTree>)>,
    /// let go far enough: once the move ends, this desktop is the one showing.
    pub(crate) commit: Option<usize>,
    /// a gesture underway: how far the fingers have gone, and whether sideways (decided after a few points).
    gone: (f32, f32),
    sideways: Option<bool>,
    touching: bool,
    /// how fast the fingers were going, in points per second, and when they last moved.
    speed: f32,
    last: Option<std::time::Instant>,
    /// when the fingers last lifted: the coasting that follows is the trackpad's, not a new swipe.
    lifted: Option<std::time::Instant>,
    /// each desktop's panes as they looked when it was last left, by desktop id:
    /// what slides in, so arriving changes nothing you can see.
    pub(crate) shots: HashMap<String, std::sync::Arc<RenderImage>>,
    /// the browser's page, blurred, by desktop id, and where on its picture it goes:
    /// a native page isn't in a picture of the window, and white reads as broken.
    pub(crate) veils: HashMap<String, (std::sync::Arc<RenderImage>, [f32; 4])>,
    /// since when the page's blur has been asked for: the page stays up until it's here (or a moment has passed).
    pub(crate) veiling: Option<std::time::Instant>,
    /// where each desktop's tab, and the row of them, were drawn: the
    /// highlight goes from one tab to the next as the swipe does.
    pub(crate) tabs: std::rc::Rc<std::cell::RefCell<(Option<Bounds<Pixels>>, Vec<Option<Bounds<Pixels>>>)>>,
}

impl DeskDrag {
    pub(crate) fn new() -> Self {
        Self { x: Spring::points(0.0), shift: Default::default(), peek: None, commit: None, gone: (0.0, 0.0), sideways: None, touching: false, speed: 0.0, last: None, lifted: None, shots: HashMap::new(), veils: HashMap::new(), veiling: None, tabs: Default::default() }
    }

    /// something to draw: the panes are off their place, or a desktop is coming in.
    pub(crate) fn active(&self) -> bool {
        self.peek.is_some() || self.x.value().abs() > 0.25 || self.x.is_moving()
    }

    /// the native page can step aside now: its blur is in its place, or it took too long.
    pub(crate) fn veil_ready(&self, have: bool) -> bool {
        have || self.veiling.is_none_or(|t| t.elapsed() > std::time::Duration::from_millis(150))
    }
}

/// past this share of the width, letting go carries on to the next desktop.
const FAR_ENOUGH: f32 = 0.3;
/// a flick this fast carries on however short it was.
const FLICK: f32 = 500.0;

/// a chat being carried, from the overview to a desktop.
#[derive(Clone)]
pub(crate) struct ChatDrag(pub(crate) SessionId);

/// what follows the pointer while a chat is carried: its name.
pub(crate) struct ChatGhost(pub(crate) SharedString);

impl Render for ChatGhost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use crate::theme::{self, Type, Typeset as _};
        let _ = cx;
        div().px_3().py_2().rounded(px(theme::R_TILE)).bg(gpui_kit::black().opacity(0.72)).text_color(gpui_kit::white()).set(Type::Callout).child(self.0.clone())
    }
}

/// the desktop a layout shows: its id. a file without desktops is "d1", the
/// id its layout gets when a second desktop is made, so nothing is lost then.
pub(crate) fn desktop_key(ui: &crate::ui::Ui) -> String {
    ui.desktops.get(ui.desktop).map_or_else(|| "d1".to_owned(), |d| d.id.clone())
}

impl Shell {
    /// everything the window keeps of the desktop it's leaving.
    pub(crate) fn leave_desktop(&mut self, key: String, cx: &mut Context<Self>) {
        let open = self.model.open.as_ref().map(|o| o.id).or(self.awaiting);
        // the draft stays with its chat, for wherever the chat shows next.
        let text = self.composer.read(cx).value().to_string();
        if let Some(o) = open {
            if text.trim().is_empty() {
                self.drafts.remove(&o);
            } else {
                self.drafts.insert(o, text);
            }
        }
        if let Some(o) = self.model.open.take() {
            self.model.others.insert(o.id, o);
        }
        self.awaiting = None;
        let scroller = std::mem::replace(&mut self.scroller, cx.new(|cx| MessageScrollerState::new(0, cx)));
        let stashed = Stashed {
            open,
            chat_pane: self.chat_pane.take(),
            views: std::mem::take(&mut self.views),
            scroller,
            pane_focus: self.pane_focus.take(),
            show_new: self.show_new,
            fresh: false,
        };
        self.stashed.insert(key, stashed);
        self.answers.clear();
        self.prompt_open.snap(0.0);
        self.prompt_ghost.clear();
    }

    /// put back what the desktop had, or, the first time, the chat it had
    /// open last, else the latest in its projects, else a new chat.
    pub(crate) fn enter_desktop(&mut self, key: &str, cx: &mut Context<Self>) {
        match self.stashed.remove(key) {
            Some(st) => {
                self.scroller = st.scroller;
                self.chat_pane = st.chat_pane;
                self.views = st.views;
                self.pane_focus = st.pane_focus;
                self.show_new = st.show_new;
                match st.open {
                    Some(s) => self.switch_open(s, !st.fresh, cx),
                    None => self.draft_to_set = Some(String::new()),
                }
            }
            None => {
                let saved = self.ui.desktops.get(self.ui.desktop).and_then(|d| d.open.as_deref()).and_then(|s| s.parse::<uuid::Uuid>().ok()).map(SessionId);
                let scope = self.model.scope.clone();
                let latest = self
                    .model
                    .sessions
                    .iter()
                    .filter(|s| scope.is_empty() || scope.contains(&crate::model::project_name(s)))
                    .max_by_key(|s| s.updated_ms.max(s.created_ms))
                    .map(|s| s.id);
                match saved.filter(|s| self.model.sessions.iter().any(|x| x.id == *s)).or(latest) {
                    Some(s) => {
                        self.show_new = false;
                        self.switch_open(s, false, cx);
                    }
                    None => {
                        self.show_new = true;
                        self.draft_to_set = Some(String::new());
                    }
                }
            }
        }
        self.sync_follow();
    }

    /// the chats on desktops you aren't looking at, still followed.
    pub(crate) fn stashed_sessions(&self) -> Vec<SessionId> {
        let mut out = Vec::new();
        for st in self.stashed.values() {
            out.extend(st.open);
            out.extend(st.views.values().map(|v| v.session));
        }
        out
    }

    /// does something on a desktop you aren't looking at want you: a chat it
    /// shows, or on a desktop that lists some projects, anything in them.
    fn desktop_wants(&self, at: usize) -> bool {
        let Some(d) = self.ui.desktops.get(at) else { return false };
        let shown: Vec<SessionId> = match self.stashed.get(&d.id) {
            Some(st) => st.open.into_iter().chain(st.views.values().map(|v| v.session)).collect(),
            None => d.open.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok()).map(SessionId).into_iter().collect(),
        };
        self.model.sessions.iter().filter(|s| s.attention.is_some()).any(|s| {
            shown.contains(&s.id) || (!d.scope.is_empty() && d.scope.contains(&crate::model::project_name(s)))
        })
    }

    /// the desktops by name, the one showing filled; + makes another.
    pub(crate) fn desktops_el(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::theme::{self, Type, Typeset as _};
        use gpui_kit::component::button::{Button, ButtonVariants as _};
        use gpui_kit::component::{Icon, Sizable as _, h_flex};
        use gpui_kit::prelude::FluentBuilder as _;
        let p = self.palette.get();
        let add = Button::new("desktop-new")
            .ghost()
            .small()
            .icon(Icon::empty().path("icons/plus.svg"))
            .tooltip("New desktop")
            .on_click(cx.listener(|this, _, window, cx| this.run_key("desktop.new", json!({}), window, cx)));
        // one desktop: nothing to switch between, just the way to make another.
        if self.ui.desktops.len() < 2 {
            return h_flex().id("desktops").test_support().justify_end().child(add).into_any_element();
        }
        let tabs = self.desk.tabs.clone();
        tabs.borrow_mut().1.resize(self.ui.desktops.len(), None);
        // the highlight: on the desktop showing, or partway to the one a swipe is bringing in.
        let lit = {
            let t = tabs.borrow();
            let at = |i: usize| t.1.get(i).copied().flatten();
            let (row, from) = (t.0, at(self.ui.desktop));
            let width = self.pane_area.get().map_or(800.0, |b| f32::from(b.size.width)).max(1.0);
            let to = self.desk.peek.as_ref().and_then(|(n, _)| at(*n));
            let k = (self.desk.x.value().abs() / width).clamp(0.0, 1.0);
            row.zip(from).map(|(row, from)| {
                let lerp = |a: Pixels, b: Pixels| a + (b - a) * k;
                let b = match to {
                    Some(to) => Bounds::new(point(lerp(from.origin.x, to.origin.x), from.origin.y), size(lerp(from.size.width, to.size.width), from.size.height)),
                    None => from,
                };
                (b.origin.x - row.origin.x, b.origin.y - row.origin.y, b.size.width, b.size.height)
            })
        };
        // which tab reads as the one showing: the nearer end of the swipe.
        let lead = match &self.desk.peek {
            Some((n, _)) if self.desk.x.value().abs() > self.pane_area.get().map_or(800.0, |b| f32::from(b.size.width)) / 2.0 => *n,
            _ => self.ui.desktop,
        };
        let row_cell = tabs.clone();
        let mut row = h_flex()
            .id("desktops")
            .test_support()
            .relative()
            .gap(theme::z(2.))
            .items_center()
            .overflow_x_scroll()
            .child(canvas(move |b, _, _| row_cell.borrow_mut().0 = Some(b), |_, _, _, _| {}).absolute().inset_0())
            .children(lit.map(|(x, y, w, h)| div().absolute().left(x).top(y).w(w).h(h).rounded(theme::z(theme::R_CONTROL)).bg(p.selection)));
        for (i, d) in self.ui.desktops.iter().enumerate() {
            let on = i == lead;
            let measured = lit.is_some();
            let cell = tabs.clone();
            if self.renaming_desktop == Some(i) {
                row = row.child(div().w(theme::z(120.)).child(gpui_kit::component::input::Input::new(&self.desktop_rename).small()));
                continue;
            }
            let wants = !on && self.desktop_wants(i);
            let tip = if i < 9 { format!("{}   ⌘{}", d.name, i + 1) } else { d.name.clone() };
            row = row.child(
                h_flex()
                    .id(SharedString::from(format!("desktop-{}", i + 1)))
                    .test_support()
                    .aria_label(SharedString::from(format!("{}{}", d.name, if on { ", showing" } else { "" })))
                    .flex_shrink_0()
                    .h(theme::z(24.))
                    .px(theme::z(9.))
                    .gap(theme::z(5.))
                    .items_center()
                    .rounded(theme::z(theme::R_CONTROL))
                    .cursor_pointer()
                    .set(Type::Callout)
                    .relative()
                    // until the tabs have been measured once, the one showing carries its own highlight.
                    .when(on && !measured, |d| d.bg(p.selection))
                    .when(on, |d| d.text_color(p.label))
                    .when(!on, |d| d.text_color(p.secondary).hover(|d| d.bg(p.hover).text_color(p.label)))
                    .child(canvas(move |b, _, _| { if let Some(slot) = cell.borrow_mut().1.get_mut(i) { *slot = Some(b); } }, |_, _, _, _| {}).absolute().inset_0())
                    .child(div().max_w(theme::z(96.)).truncate().child(d.name.clone()))
                    // something on it wants you: the blue every "waiting" in tessel is.
                    .when(wants, |d| d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(theme::info(&p))))
                    .tooltip({
                        let tip = tip.clone();
                        move |w, cx| gpui_kit::component::tooltip::Tooltip::new(tip.clone()).build(w, cx)
                    })
                    .on_click(cx.listener(move |this, ev: &ClickEvent, window, cx| {
                        if ev.click_count() >= 2 {
                            let name = this.ui.desktops.get(i).map(|d| d.name.clone()).unwrap_or_default();
                            this.desktop_rename.update(cx, |s, cx| {
                                s.set_value(name, window, cx);
                                s.focus(window, cx);
                            });
                            this.renaming_desktop = Some(i);
                            cx.notify();
                        } else if i != this.ui.desktop {
                            this.use_desktop(i, window, cx);
                        }
                    })),
            );
        }
        h_flex().gap(theme::z(4.)).items_center().child(div().flex_1().min_w_0().child(row)).child(add).into_any_element()
    }

    /// the desktop a chat belongs on: the one showing it (this one first),
    /// else one that lists its project, else this one if it lists everything.
    /// none: stay, and show it here.
    pub(crate) fn desktop_for(&self, session: SessionId) -> Option<usize> {
        if self.ui.desktops.len() < 2 {
            return None;
        }
        let here = self.model.open.as_ref().is_some_and(|o| o.id == session) || self.in_a_view(session);
        if here {
            return Some(self.ui.desktop);
        }
        if let Some(at) = self.ui.desktops.iter().position(|d| self.stashed.get(&d.id).is_some_and(|st| st.open == Some(session) || st.views.values().any(|v| v.session == session))) {
            return Some(at);
        }
        let project = self.model.sessions.iter().find(|s| s.id == session).map(crate::model::project_name)?;
        let lists = |d: &crate::ui::Desktop| d.scope.is_empty() || d.scope.contains(&project);
        if self.ui.desktops.get(self.ui.desktop).is_some_and(lists) {
            return Some(self.ui.desktop);
        }
        self.ui.desktops.iter().position(|d| d.scope.contains(&project)).or_else(|| self.ui.desktops.iter().position(lists))
    }

    /// show a chat on a desktop: in its focused pane, now if it's showing,
    /// else for when you go there. a desktop that doesn't exist yet (`at` past
    /// the end) is made for it.
    pub(crate) fn show_on_desktop(&mut self, session: SessionId, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        if at >= self.ui.desktops.len().max(1) {
            let name = self.model.sessions.iter().find(|s| s.id == session).map(crate::model::project_name);
            self.new_desktop(name, None, false, Vec::new(), window, cx);
            self.show_new = false;
            self.open(session, cx);
            return;
        }
        if at == self.ui.desktop {
            self.show_new = false;
            self.open(session, cx);
            return;
        }
        let id = self.ui.desktops[at].id.clone();
        if let Some(st) = self.stashed.get_mut(&id) {
            st.open = Some(session);
            st.show_new = false;
            st.fresh = true;
        }
        let text = session.0.to_string();
        self.edit_desktop(at, |d| {
            d.insert("open".into(), json!(text));
        }, window, cx);
        self.sync_follow();
        cx.notify();
    }

    /// the desktops across the top of the overview: each with the chat it has
    /// open. click one to go there; drop a chat on one to show it there, or
    /// on + for a desktop of its own.
    pub(crate) fn overview_desktops(&self, cx: &mut Context<Self>) -> AnyElement {
        use crate::theme::{self, Type, Typeset as _};
        use gpui_kit::component::{Icon, h_flex, v_flex};
        use gpui_kit::prelude::FluentBuilder as _;
        let p = self.palette.get();
        let count = self.ui.desktops.len().max(1);
        let mut row = h_flex().id("overview-desktops").test_support().gap(theme::z(6.)).items_center();
        for i in 0..count {
            let on = i == self.ui.desktop;
            let name = self.ui.desktops.get(i).map_or_else(|| "Main".to_owned(), |d| d.name.clone());
            let chat = if on {
                self.model.open.as_ref().map(|o| o.id)
            } else {
                let d = &self.ui.desktops[i];
                self.stashed.get(&d.id).and_then(|st| st.open).or_else(|| d.open.as_deref().and_then(|s| s.parse::<uuid::Uuid>().ok()).map(SessionId))
            };
            let chat = chat.and_then(|c| self.model.sessions.iter().find(|s| s.id == c)).map_or_else(|| "No chat".to_owned(), |s| s.goal.clone());
            let wants = !on && self.desktop_wants(i);
            row = row.child(
                v_flex()
                    .id(SharedString::from(format!("overview-desktop-{}", i + 1)))
                    .test_support()
                    .w(theme::z(150.))
                    .px(theme::z(10.))
                    .py(theme::z(5.))
                    .rounded(theme::z(theme::R_CONTROL + 2.))
                    .border_1()
                    .border_color(if on { p.accent.opacity(0.5) } else { p.separator })
                    .when(on, |d| d.bg(p.selection))
                    .cursor_pointer()
                    .hover(|d| d.bg(p.hover))
                    .drag_over::<ChatDrag>(move |d, _, _, _| d.border_color(p.accent).bg(p.accent.opacity(0.08)))
                    .child(
                        h_flex()
                            .gap(theme::z(5.))
                            .items_center()
                            .child(div().min_w_0().truncate().set(Type::Headline).text_color(p.label).child(name))
                            .when(wants, |d| d.child(div().size(px(6.)).flex_shrink_0().rounded_full().bg(theme::info(&p)))),
                    )
                    .child(div().min_w_0().truncate().set(Type::Subheadline).text_color(p.secondary).child(chat))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if i != this.ui.desktop {
                            this.use_desktop(i, window, cx);
                        }
                        this.close_overview(None, cx);
                    }))
                    .on_drop(cx.listener(move |this, drag: &ChatDrag, window, cx| this.show_on_desktop(drag.0, i, window, cx))),
            );
        }
        row = row.child(
            h_flex()
                .id("overview-desktop-new")
                .test_support()
                .h(theme::z(40.))
                .px(theme::z(12.))
                .gap(theme::z(6.))
                .items_center()
                .rounded(theme::z(theme::R_CONTROL + 2.))
                .border_1()
                .border_dashed()
                .border_color(p.separator)
                .cursor_pointer()
                .hover(|d| d.bg(p.hover))
                .drag_over::<ChatDrag>(move |d, _, _, _| d.border_color(p.accent).bg(p.accent.opacity(0.08)))
                .set(Type::Callout)
                .text_color(p.secondary)
                .child(Icon::empty().path("icons/plus.svg").size(theme::z(13.)))
                .child("Desktop")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run_key("desktop.new", json!({}), window, cx);
                    this.close_overview(None, cx);
                }))
                .on_drop(cx.listener(move |this, drag: &ChatDrag, window, cx| {
                    this.show_on_desktop(drag.0, usize::MAX, window, cx);
                    this.close_overview(None, cx);
                })),
        );
        row.into_any_element()
    }

    /// two fingers sideways on the sidebar change desktop, as they change
    /// spaces on the mac: towards the left for the next one. on a trackpad the
    /// panes follow the fingers and the next desktop comes in beside them;
    /// stop halfway and hold, go back, or let go past a third (or with a
    /// flick) to carry on. a wheel, which can't say where a swipe starts or
    /// ends, switches once it has gone far enough. up and down is the list's.
    pub(crate) fn sidebar_swipe(&mut self, ev: &ScrollEvent, window: &mut Window, cx: &mut Context<Self>) {
        const ENOUGH: f32 = 60.0;
        if self.ui.desktops.len() < 2 {
            return;
        }
        let now = std::time::Instant::now();
        let d = match ev.delta {
            ScrollDelta::Pixels(p) => (f32::from(p.x), f32::from(p.y)),
            ScrollDelta::Lines(l) => (l.x * 20.0, l.y * 20.0),
        };
        match ev.touch_phase {
            TouchPhase::Started => {
                let g = &mut self.desk;
                g.touching = true;
                g.gone = (0.0, 0.0);
                g.sideways = None;
                g.speed = 0.0;
                g.last = Some(now);
                // caught mid-flight: carry on from where it is, toward neither end yet.
                g.commit = None;
                let at = g.x.value();
                g.x.snap(at);
                self.desk_follow(d, now, window, cx);
            }
            TouchPhase::Moved if self.desk.touching => self.desk_follow(d, now, window, cx),
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.desk_follow(d, now, window, cx);
                self.desk_let_go(now);
                cx.notify();
            }
            // the trackpad coasting after a swipe: it's already decided.
            TouchPhase::Moved if self.desk.lifted.is_some_and(|t| now.duration_since(t).as_millis() < 900) => {
                self.desk.lifted = Some(now);
            }
            // a wheel: no start or end, so a pause is what ends a swipe.
            TouchPhase::Moved => {
                let (mut gone, mut spent, last, _) = self.swipe;
                if last.is_none_or(|t| now.duration_since(t).as_millis() > 300) {
                    (gone, spent) = (0.0, false);
                }
                if !spent && d.0.abs() > d.1.abs() {
                    gone += d.0;
                    if gone.abs() >= ENOUGH {
                        spent = true;
                        let next = if gone < 0.0 { "desktop.next" } else { "desktop.prev" };
                        self.run_key(next, json!({}), window, cx);
                    }
                }
                self.swipe = (gone, spent, Some(now), false);
            }
        }
    }

    /// the fingers moved by `d`: the panes go with them, sideways swipes only.
    fn desk_follow(&mut self, d: (f32, f32), now: std::time::Instant, window: &mut Window, cx: &mut Context<Self>) {
        let width = self.pane_area.get().map_or(800.0, |b| f32::from(b.size.width)).max(1.0);
        let g = &mut self.desk;
        g.gone.0 += d.0;
        g.gone.1 += d.1;
        if g.sideways.is_none() && g.gone.0.abs() + g.gone.1.abs() > 6.0 {
            g.sideways = Some(g.gone.0.abs() > g.gone.1.abs());
        }
        if g.sideways != Some(true) {
            return;
        }
        if let Some(last) = g.last {
            let dt = now.duration_since(last).as_secs_f32();
            if dt > 0.0 && d.0 != 0.0 {
                // smoothed, so one odd event doesn't decide a flick.
                g.speed = g.speed * 0.4 + (d.0 / dt.max(0.004)) * 0.6;
            }
        }
        g.last = Some(now);
        // where the panes would be, then held back where there's nothing to go to.
        let want = g.x.value() + d.0;
        let at = self.ui.desktop;
        let toward = if want < 0.0 { at.checked_add(1).filter(|n| *n < self.ui.desktops.len()) } else { at.checked_sub(1) };
        let x = match toward {
            Some(_) => want.clamp(-width, width),
            // past the end: it gives a little, and less the further it goes.
            None => {
                let pull = want.signum() * (want.abs() * 0.25).min(60.0);
                if (g.x.value() - pull).abs() > 0.1 { pull } else { g.x.value() }
            }
        };
        self.desk.x.snap(x);
        self.desk.shift.set(x);
        let wanted = toward.filter(|_| x.abs() > 0.5);
        // leaving from rest: this desktop as it is, for coming back to, and its page, blurred.
        if self.desk.peek.is_none() && wanted.is_some() && self.desk.commit.is_none() {
            self.shoot_desktop(window);
            self.veil_page(cx);
        }
        if self.desk.peek.as_ref().map(|p| p.0) != wanted {
            self.desk.peek = wanted.map(|n| (n, self.desk_panes(n)));
        }
        cx.notify();
    }

    /// the fingers lifted: on to the desktop coming in, or back.
    fn desk_let_go(&mut self, now: std::time::Instant) {
        let width = self.pane_area.get().map_or(800.0, |b| f32::from(b.size.width)).max(1.0);
        let g = &mut self.desk;
        g.touching = false;
        g.lifted = Some(now);
        let x = g.x.value();
        // a flick only counts if the fingers were still moving when they lifted.
        let speed = if g.last.is_some_and(|t| now.duration_since(t).as_millis() < 80) { g.speed } else { 0.0 };
        let flicked = speed.abs() > FLICK;
        let going = g.peek.as_ref().map(|p| p.0).filter(|_| {
            // past a third carries on, unless flicked back; a flick onward carries on however short.
            let far = x.abs() > width * FAR_ENOUGH && !(flicked && speed.signum() != x.signum());
            far || (flicked && speed.signum() == x.signum())
        });
        match going {
            Some(n) => {
                g.commit = Some(n);
                g.x.fling(x.signum() * width, speed);
            }
            None => {
                g.commit = None;
                g.x.fling(0.0, speed);
            }
        }
    }

    /// the panes of desktop `n`, as its layout lays them out.
    fn desk_panes(&self, n: usize) -> Option<crate::ui::PaneTree> {
        let mut file = self.ui_file();
        let id = self.ui.desktops.get(n)?.id.clone();
        file.insert("desktop".into(), json!(id));
        let ui = crate::ui::parse(&Value::Object(file).to_string()).ok()?;
        ui.root_panes().cloned()
    }

    /// once a frame: a swipe that carried on, landed, shows its desktop.
    pub(crate) fn sync_desk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.desk.shift.set(self.desk.x.value());
        if self.desk.touching || self.desk.x.is_moving() {
            return;
        }
        match self.desk.commit {
            // landed: the desktop coming in stays drawn where it is until the
            // switch has happened, then its panes take its place in the same
            // frame, already where they belong. nothing flashes, nothing moves.
            Some(n) if self.ui.desktop != n => self.use_desktop(n, window, cx),
            Some(_) => {
                let g = &mut self.desk;
                g.commit = None;
                g.x.snap(0.0);
                g.shift.set(0.0);
                g.peek = None;
                g.veiling = None;
                self.desktop_slide.snap(0.0);
                self.settle_panes();
                cx.notify();
            }
            None if self.desk.x.value() == 0.0 && self.desk.peek.is_some() => {
                self.desk.peek = None;
                self.desk.veiling = None;
            }
            None => {}
        }
    }

    /// keep a picture of the panes as they are now (the last frame drawn), for this desktop.
    pub(crate) fn shoot_desktop(&mut self, window: &mut Window) {
        let Some(area) = self.pane_area.get() else { return };
        let Ok(frame) = window.render_to_image() else { return };
        let scale = window.scale_factor();
        // only this window's frame will do (asked from another window's turn, it may not be).
        let whole = window.viewport_size();
        if (frame.width() as f32 - f32::from(whole.width) * scale).abs() > 2.0 {
            return;
        }
        let (x, y) = ((f32::from(area.origin.x) * scale).max(0.0) as u32, (f32::from(area.origin.y) * scale).max(0.0) as u32);
        let (w, h) = ((f32::from(area.size.width) * scale) as u32, (f32::from(area.size.height) * scale) as u32);
        if w == 0 || h == 0 || x + w > frame.width() || y + h > frame.height() {
            return;
        }
        let mut shot = image::imageops::crop_imm(&frame, x, y, w, h).to_image();
        // what's painted is bgra.
        for px in shot.chunks_exact_mut(4) {
            px.swap(0, 2);
        }
        let key = desktop_key(&self.ui);
        // the picture it replaces lets go of its place on the gpu.
        if let Some(old) = self.desk.shots.insert(key, std::sync::Arc::new(RenderImage::new(vec![image::Frame::new(shot)]))) {
            let _ = window.drop_image(old);
        }
    }

    /// the desktop coming in: its panes as outlines, each with what it shows.
    pub(crate) fn desk_peek_el(&self, pad_left: f32, pad_top: f32, w: f32, h: f32) -> Option<AnyElement> {
        let (n, tree) = self.desk.peek.as_ref()?;
        let p = self.palette.get();
        let x = self.desk.x.value();
        // beside the panes, on the side they're leaving from.
        let left = pad_left + if x < 0.0 { x + w } else { x - w };
        let rects = tree.as_ref().map(|t| self.pane_rects(t, crate::model::Rect { x: 0.0, y: 0.0, w, h })).unwrap_or_default();
        let leaves: Vec<crate::ui::Pane> = tree.as_ref().map(|t| {
            let mut out = Vec::new();
            t.leaves(&mut out);
            out.into_iter().cloned().collect()
        }).unwrap_or_default();
        let name = self.ui.desktops.get(*n).map(|d| d.name.clone()).unwrap_or_default();
        let mut layer = div().absolute().top(px(pad_top)).left(px(left)).w(px(w)).h(px(h));
        // as it looked when it was left: exactly what arriving will show.
        if let Some(shot) = self.ui.desktops.get(*n).and_then(|d| self.desk.shots.get(&d.id)).cloned() {
            let veil = self.ui.desktops.get(*n).and_then(|d| self.desk.veils.get(&d.id)).cloned();
            let pic = canvas(|_, _, _| (), move |bounds, (), window, _| {
                let _ = window.paint_image(bounds, bounds, Corners::default(), shot.clone(), 0, false);
                // its page, blurred, where the picture has only the white behind it.
                if let Some((veil, [x, y, w, h])) = &veil {
                    let at = Bounds::new(point(bounds.origin.x + px(*x), bounds.origin.y + px(*y)), size(px(*w), px(*h)));
                    let r = px(crate::theme::R_PANEL - 1.0);
                    let corners = Corners { top_left: px(0.), top_right: px(0.), bottom_left: r, bottom_right: r };
                    let _ = window.paint_image(at, at, corners, veil.clone(), 0, false);
                }
            })
            .size_full();
            return Some(layer.child(pic).into_any_element());
        }
        if rects.is_empty() {
            layer = layer.child(div().size_full().flex().items_center().justify_center().set(crate::theme::Type::Title).text_color(p.secondary).child(name));
        }
        for (id, r) in rects {
            let module = leaves.iter().find(|l| l.id == id).and_then(|l| l.module);
            let (title, icon) = module.map_or(("".to_owned(), "square"), |m| (m.title().to_owned(), m.icon()));
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(r.x))
                    .top(px(r.y))
                    .w(px(r.w))
                    .h(px(r.h))
                    .rounded(px(crate::theme::R_PANEL))
                    .border_1()
                    .border_color(p.separator)
                    .bg(p.window)
                    .overflow_hidden()
                    .child(
                        gpui_kit::component::h_flex()
                            .h(px(super::panes::HEADER_H))
                            .px(px(12.))
                            .gap(px(8.))
                            .border_b_1()
                            .border_color(p.separator)
                            .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(14.)).text_color(p.tertiary))
                            .child(div().set(crate::theme::Type::Headline).text_color(p.secondary).child(title)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h(px((r.h - super::panes::HEADER_H).max(0.0)))
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(28.)).text_color(p.quaternary)),
                    ),
            );
        }
        Some(layer.into_any_element())
    }

    /// ⌘1…⌘9: that desktop, when there is one.
    pub(crate) fn go_desktop(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if n >= 1 && n <= self.ui.desktops.len() && n - 1 != self.ui.desktop {
            self.use_desktop(n - 1, window, cx);
        }
    }

    /// a quick fade as a desktop's panes arrive: no movement, so flicking
    /// between desktops never makes anyone dizzy.
    pub(crate) fn slide_desktop(&mut self) {
        const ARRIVE: crate::motion::Preset = crate::motion::Preset { duration: 0.14, bounce: 0.0 };
        self.desktop_slide = Spring::unit(1.0);
        self.desktop_slide.to(0.0, ARRIVE);
    }

    // ---- the list in ui.json ---------------------------------------------------------

    /// the file's desktops, made if it has none yet: the layout in use becomes
    /// "Main", so nothing on screen changes by gaining a second desktop.
    fn desktops_in(file: &mut Map<String, Value>) -> &mut Vec<Value> {
        if !matches!(file.get("desktops"), Some(Value::Array(a)) if !a.is_empty()) {
            file.insert("desktops".into(), json!([{"id": "d1", "name": "Main"}]));
            file.insert("desktop".into(), json!("d1"));
        }
        match file.get_mut("desktops") {
            Some(Value::Array(a)) => a,
            _ => unreachable!("just made"),
        }
    }

    /// a desktop named by its number (from 1), its name or its id; none: the one showing.
    pub(crate) fn desktop_index(&self, which: Option<&Value>) -> Result<usize, String> {
        let list = &self.ui.desktops;
        let names = || list.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ");
        match which {
            None | Some(Value::Null) => Ok(self.ui.desktop),
            Some(Value::Number(n)) => {
                let n = n.as_u64().unwrap_or(0) as usize;
                if n >= 1 && n <= list.len().max(1) {
                    Ok(n - 1)
                } else {
                    Err(format!("there's no desktop {n}. there are {}", list.len().max(1)))
                }
            }
            Some(Value::String(s)) => list
                .iter()
                .position(|d| d.id == *s || d.name.eq_ignore_ascii_case(s))
                .or_else(|| s.parse::<usize>().ok().filter(|n| *n >= 1 && *n <= list.len()).map(|n| n - 1))
                .ok_or_else(|| format!("there's no desktop \"{s}\". desktops: {}", names())),
            Some(other) => Err(format!("desktop: expected a number, a name or an id, got {other}")),
        }
    }

    /// show desktop `to`, remembering the chat the one you leave had open.
    pub(crate) fn use_desktop(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.model.open.as_ref().map(|o| o.id.0.to_string());
        let from = self.ui.desktop;
        self.edit_ui(
            |f| {
                let list = Self::desktops_in(f);
                if let Some(Value::Object(d)) = list.get_mut(from) {
                    match &open {
                        Some(id) => {
                            d.insert("open".into(), json!(id));
                        }
                        None => {
                            d.remove("open");
                        }
                    }
                }
                if let Some(id) = list.get(to).and_then(|d| d.get("id")).cloned().or_else(|| Some(json!(format!("d{}", to + 1)))) {
                    f.insert("desktop".into(), id);
                }
            },
            window,
            cx,
        );
    }

    /// a new desktop after the last, shown at once. `copy` starts it with the
    /// layout in use; otherwise `preset`, else the default layout.
    pub(crate) fn new_desktop(&mut self, name: Option<String>, preset: Option<String>, copy: bool, scope: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Value {
        let shared_root = self.ui_file().get("root").cloned();
        let mut made = Value::Null;
        let open = self.model.open.as_ref().map(|o| o.id.0.to_string());
        let from = self.ui.desktop;
        self.edit_ui(
            |f| {
                // the panes showing now, in case the new desktop copies them.
                let showing = Self::desktops_in(f)
                    .get(from)
                    .and_then(Value::as_object)
                    .and_then(crate::ui::desktop_layout)
                    .or_else(|| shared_root.as_ref().and_then(crate::ui::panes_layout).cloned());
                let list = Self::desktops_in(f);
                if let (Some(Value::Object(d)), Some(id)) = (list.get_mut(from), &open) {
                    d.insert("open".into(), json!(id));
                }
                let taken: Vec<String> = list.iter().filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_owned)).collect();
                let id = (1..).map(|n| format!("d{n}")).find(|id| !taken.contains(id)).expect("there's always a free one");
                let n = list.len() + 1;
                let mut d = Map::new();
                d.insert("id".into(), json!(id));
                d.insert("name".into(), json!(name.clone().unwrap_or_else(|| format!("Desktop {n}"))));
                if !scope.is_empty() {
                    d.insert("scope".into(), json!(scope));
                }
                let from_preset = |name: &str| {
                    crate::ui::preset(name).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("root").and_then(crate::ui::panes_layout).cloned())
                };
                let layout = match (copy, preset.as_deref()) {
                    (true, _) => showing,
                    (_, Some(p)) => from_preset(p),
                    _ => from_preset("default"),
                };
                if let Some(layout) = layout {
                    d.insert("layout".into(), layout);
                }
                list.push(Value::Object(d));
                f.insert("desktop".into(), json!(id));
                made = json!({"desktop": id, "number": n});
            },
            window,
            cx,
        );
        made
    }

    /// change one desktop's entry in the file.
    pub(crate) fn edit_desktop(&mut self, at: usize, change: impl FnOnce(&mut Map<String, Value>), window: &mut Window, cx: &mut Context<Self>) {
        self.edit_ui(
            |f| {
                if let Some(Value::Object(d)) = Self::desktops_in(f).get_mut(at) {
                    change(d);
                }
            },
            window,
            cx,
        );
    }

    /// forget a desktop. its chats keep running; they're a click away on any other.
    pub(crate) fn close_desktop(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.ui.desktops.len() <= 1 {
            return Err("this is the only desktop".into());
        }
        let showing = self.ui.desktop;
        let gone = self.ui.desktops[at].id.clone();
        self.edit_ui(
            |f| {
                let list = Self::desktops_in(f);
                if at < list.len() {
                    list.remove(at);
                }
                // the one beside it shows, as a closed tab's neighbour does.
                if at == showing {
                    let next = at.min(list.len().saturating_sub(1));
                    if let Some(id) = list.get(next).and_then(|d| d.get("id")).cloned() {
                        f.insert("desktop".into(), id);
                    }
                }
            },
            window,
            cx,
        );
        self.stashed.remove(&gone);
        self.sync_follow();
        Ok(())
    }

    /// the panes to change: the showing desktop's own, made from the shared
    /// ones the first time it's changed; with no desktops, the file's.
    pub(crate) fn panes_to_edit(file: &mut Map<String, Value>, showing: usize, default_root: Option<Value>) -> Option<&mut Value> {
        if !matches!(file.get("desktops"), Some(Value::Array(a)) if !a.is_empty()) {
            if !file.contains_key("root") {
                file.insert("root".into(), default_root?);
                file.remove("preset");
            }
            return crate::ui::panes_layout_mut(file.get_mut("root")?);
        }
        let shared = file.get("root").and_then(crate::ui::panes_layout).cloned().or_else(|| default_root.as_ref().and_then(crate::ui::panes_layout).cloned());
        let Some(Value::Object(d)) = Self::desktops_in(file).get_mut(showing) else { return None };
        if !d.contains_key("layout") {
            let layout = crate::ui::desktop_layout(d).or(shared)?;
            d.remove("root");
            d.remove("preset");
            d.insert("layout".into(), layout);
        }
        d.get_mut("layout")
    }
}
