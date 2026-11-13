//! the Browser pane: the open chat's page, live. it's the page the agent
//! drives with its tools, so what it does shows here as it does it (a label
//! saying what, a ring where it clicked), and you can use the page too: click,
//! scroll, type. Pick points at a part of the page and puts it in your next
//! message: a picture of it, its selector and its HTML, for you to say what
//! should change.
//!
//! on the Mac the page is a native web view in the window (see `webview`),
//! and the agent's acts are relayed to it by the daemon and done here. where
//! that can't be (tests, other platforms, `TESSEL_BROWSER_STREAM=1`), the
//! daemon runs the browser and sends frames while a pane watches; then this
//! only shows them and passes your input back.

use super::*;
use crate::webview::{self, WebView};
use base64::Engine as _;
use gpui_kit::component::Icon;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_proto::{BrowserAct, BrowserState, MouseKind, Picked, read_script, url_of};

/// a page shown natively.
pub(crate) struct Native {
    web: WebView,
}

thread_local! {
    /// where menus and other floating things are this frame: a native page
    /// over the window's content would hide them, so it steps aside.
    static FLOATS: std::cell::RefCell<Vec<Bounds<Pixels>>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// a floating thing (a menu, a popover) is here this frame.
pub(crate) fn floating_at(b: Bounds<Pixels>) {
    FLOATS.with(|f| f.borrow_mut().push(b));
}

/// a new frame: nothing floats yet.
pub(crate) fn floats_reset() {
    FLOATS.with(|f| f.borrow_mut().clear());
}

fn float_over(b: &Bounds<Pixels>) -> bool {
    FLOATS.with(|f| f.borrow().iter().any(|o| o.intersects(b)))
}

/// something to do to a page: from the daemon (an agent's act, `call` to
/// answer; `first`, where a new page starts) or from the person.
pub(crate) struct PageCall {
    pub(crate) session: SessionId,
    pub(crate) act: BrowserAct,
    pub(crate) agent: bool,
    pub(crate) call: Option<u64>,
    pub(crate) first: Option<String>,
}

/// one chat's page, as the pane has it.
#[derive(Default)]
pub(crate) struct PageView {
    /// the frame on screen, and the next one, not yet uploaded.
    frame: Option<(Arc<RenderImage>, u32, u32)>,
    next: Option<(Vec<u8>, u32, u32)>,
    pub(crate) state: BrowserState,
    /// what the agent last did, where, and when.
    activity: Option<(String, Option<(f64, f64)>, Instant)>,
    /// where the page was drawn last frame, and the size the browser was told.
    bounds: Option<Bounds<Pixels>>,
    told: Option<(u32, u32)>,
    /// the page itself, when it's shown natively.
    native: Option<Rc<Native>>,
    /// asking the page what was picked, so it isn't asked twice at once.
    pick_asking: bool,
}

pub(crate) type Pages = HashMap<SessionId, Rc<RefCell<PageView>>>;

/// how long what the agent did stays said.
const SAID_FOR: Duration = Duration::from_secs(4);

impl Shell {
    fn page(&mut self, session: SessionId) -> Rc<RefCell<PageView>> {
        self.pages.entry(session).or_default().clone()
    }

    pub(crate) fn browser_event(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::BrowserFrame { session, bgra, width, height } => {
                self.page(session).borrow_mut().next = Some((bgra, width, height));
            }
            UiEvent::BrowserState { session, state } => {
                let open = self.model.open.as_ref().is_some_and(|o| o.id == session);
                if open && self.browser_url_from != state.url {
                    self.browser_url_from = state.url.clone();
                    self.browser_url_next = Some(state.url.clone());
                }
                self.page(session).borrow_mut().state = state;
            }
            UiEvent::BrowserActivity { session, what, at } => {
                self.page(session).borrow_mut().activity = Some((what, at, Instant::now()));
                // the agent is using the browser in visual mode: let it be seen.
                // (otherwise the layout is the person's, and stays as it is.)
                if self.watching(session) && !self.browser_visible() {
                    self.browser_show_next = true;
                }
            }
            UiEvent::BrowserPicked { session, picked } => {
                if self.model.open.as_ref().is_some_and(|o| o.id == session) {
                    self.add_pick(picked);
                }
            }
            UiEvent::BrowserEngine(engine) => self.browser_engine = Some(engine),
            _ => {}
        }
        cx.notify();
    }

    /// what you pointed at goes with the message you're writing, as one
    /// attachment like any other: its picture, carrying a few lines on what
    /// and where it is (in the page, and in the source) for the agent. what
    /// you type stays yours.
    fn add_pick(&mut self, p: Picked) {
        let note = p.note();
        let media_type = if p.image.is_empty() { "application/vnd.tessel.element".into() } else { "image/png".into() };
        self.attachments.push(tessel_proto::Attachment { name: p.label(), media_type, data: p.image, element: Some(note) });
        self.focus_composer_next = true;
    }

    /// the open chat is `session`, in visual mode: the person asked to watch it work.
    fn watching(&self, session: SessionId) -> bool {
        self.model.open.as_ref().is_some_and(|o| o.id == session) && self.model.open_summary().is_some_and(|s| s.visual)
    }

    pub(crate) fn browser_visible(&self) -> bool {
        self.ui.panes().iter().any(|p| p.module == Some(crate::ui::Module::Browser) && self.pane_open(p))
    }

    /// a browser pane beside the conversation, or the one there is, unfolded.
    pub(crate) fn show_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        if let Some(p) = self.ui.panes().into_iter().find(|p| p.module == Some(crate::ui::Module::Browser)) {
            let id = p.id.clone();
            if !self.pane_open(&p) {
                self.toggle_pane(&id, cx);
            }
            return Some(id);
        }
        let beside = self.ui.panes().into_iter().find(|p| p.module == Some(crate::ui::Module::Conversation)).cloned().or_else(|| self.focused_pane())?;
        let pane = self.split_pane(beside.trail, crate::ui::Axis::Row, window, cx)?;
        let trail = self.ui.panes().into_iter().find(|p| p.id == pane)?.trail.clone();
        self.set_pane_module(trail, crate::ui::Module::Browser, window, cx);
        Some(pane)
    }

    /// once a frame: the page shown is the open chat's, at the pane's size,
    /// sending frames while a pane shows it and not otherwise.
    pub(crate) fn sync_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.browser_show_next) {
            self.show_browser(window, cx);
        }
        if let Some(url) = self.browser_url_next.take() {
            self.browser_url.update(cx, |s, cx| s.set_value(url, window, cx));
        }
        if self.native_view.is_some() {
            self.sync_native(cx);
            return;
        }
        let want = self.browser_visible().then(|| self.model.open.as_ref().map(|o| o.id)).flatten();
        if want != self.browser_watching {
            if let Some(old) = self.browser_watching.take() {
                self.cmd(Cmd::Browser { session: old, act: BrowserAct::Watch { on: false } });
            }
            if let Some(s) = want {
                if self.browser_engine.is_none() && !self.browser_asked {
                    self.browser_asked = true;
                    self.cmd(Cmd::Ask { tag: "browser.engine", req: tessel_proto::Request::BrowserEngine });
                }
                self.cmd(Cmd::Browser { session: s, act: BrowserAct::Open { url: None } });
                self.cmd(Cmd::Browser { session: s, act: BrowserAct::Watch { on: true } });
                self.page(s).borrow_mut().told = None;
            }
            self.browser_watching = want;
        }
        // the page is as big as the pane: told when the pane settles at a new size.
        if let Some(s) = self.browser_watching {
            let page = self.page(s);
            let mut v = page.borrow_mut();
            if let Some(b) = v.bounds {
                let size = (f32::from(b.size.width).round() as u32, f32::from(b.size.height).round() as u32);
                if v.told != Some(size) && size.0 > 50 && size.1 > 50 && !self.panes_moving() {
                    v.told = Some(size);
                    let scale = f64::from(window.scale_factor());
                    self.cmd(Cmd::Browser { session: s, act: BrowserAct::Resize { width: size.0, height: size.1, scale } });
                }
            }
        }
    }

    /// something done to the open chat's page.
    fn act(&mut self, act: BrowserAct, cx: &mut Context<Self>) {
        if let Some(s) = self.model.open.as_ref().map(|o| o.id) {
            self.browser_act(s, act, cx);
        }
    }

    /// the pointer and keys, passed to the daemon's browser (only when it runs the page).
    fn stream_act(&mut self, act: BrowserAct) {
        if self.native_view.is_some() {
            return;
        }
        if let Some(s) = self.model.open.as_ref().map(|o| o.id) {
            self.cmd(Cmd::Browser { session: s, act });
        }
    }

    /// something done to a chat's page, wherever the page is.
    pub(crate) fn browser_act(&mut self, session: SessionId, act: BrowserAct, cx: &mut Context<Self>) {
        if self.native_view.is_none() {
            self.cmd(Cmd::Browser { session, act });
            return;
        }
        let call = PageCall { session, act, agent: false, call: None, first: None };
        if self.has_native(session) {
            self.run_page_act(call, cx);
        } else {
            // done once the page is made, at the next frame.
            self.page_calls.push(call);
            cx.notify();
        }
    }

    fn has_native(&self, session: SessionId) -> bool {
        self.pages.get(&session).is_some_and(|p| p.borrow().native.is_some())
    }

    /// a chat went: its page with it.
    pub(crate) fn forget_page(&mut self, session: SessionId) {
        if let Some(page) = self.pages.remove(&session) {
            page.borrow_mut().native.take();
        }
        self.page_opening.remove(&session);
    }

    /// once a frame, when pages are native: pages made where they're needed,
    /// what's waiting done, and only the shown chat's page on screen.
    fn sync_native(&mut self, cx: &mut Context<Self>) {
        let want = self.browser_visible().then(|| self.model.open.as_ref().map(|o| o.id)).flatten();
        // a page for the pane, starting where the workspace serves: the daemon
        // knows where that is, and answers with an Open to do here.
        if let Some(s) = want
            && !self.has_native(s)
            && self.page_opening.insert(s)
        {
            self.cmd(Cmd::Browser { session: s, act: BrowserAct::Open { url: None } });
        }
        for call in std::mem::take(&mut self.page_calls) {
            if !self.has_native(call.session) {
                let Some(first) = call.first.clone() else {
                    // the person's, before the page exists: once it does.
                    if self.page_opening.insert(call.session) {
                        self.cmd(Cmd::Browser { session: call.session, act: BrowserAct::Open { url: None } });
                    }
                    self.page_calls.push(call);
                    continue;
                };
                // a page told where to go first doesn't load the workspace's on the way.
                let start = (!matches!(&call.act, BrowserAct::Open { url: Some(_) } | BrowserAct::Navigate { .. })).then_some(first);
                if let Err(e) = self.make_page(call.session, start) {
                    match call.call {
                        Some(c) => self.cmd(Cmd::UiAnswer { call: c, result: Err(e) }),
                        None => self.error = Some(e),
                    }
                    continue;
                }
            }
            self.run_page_act(call, cx);
        }
        // only the shown chat's page is on screen; the pane shows it as it paints.
        for (s, page) in &self.pages {
            if Some(*s) != want
                && let Some(n) = &page.borrow().native
            {
                n.web.show(false);
            }
        }
    }

    fn overview_shown(&self) -> bool {
        self.overview || self.overview_fade.value() > 0.001
    }

    /// what covers the whole window now: a page steps aside for it.
    pub(crate) fn page_covered(&self) -> bool {
        self.tour.is_some() || self.palette_ui.open || self.sheet.is_some() || self.settings_open || self.overview_shown()
    }

    fn make_page(&mut self, session: SessionId, start: Option<String>) -> Result<(), String> {
        let parent = self.native_view.ok_or("the window can't show pages")?;
        let web = WebView::new(parent).ok_or("this Mac couldn't make a web view")?;
        if let Some(url) = start {
            web.load(&url_of(&url))?;
        }
        self.page(session).borrow_mut().native = Some(Rc::new(Native { web }));
        self.page_opening.remove(&session);
        Ok(())
    }

    /// do it to the page, then say how it went: to the daemon, for an agent's
    /// act; on screen, if the person's went wrong.
    fn run_page_act(&mut self, call: PageCall, cx: &mut Context<Self>) {
        let Some(page) = self.pages.get(&call.session).cloned() else { return };
        let Some(native) = page.borrow().native.clone() else { return };
        // the agent is using the browser in visual mode: let it be seen.
        if call.agent && self.watching(call.session) && !self.browser_visible() {
            self.browser_show_next = true;
            cx.notify();
        }
        // visual mode: the agent's steps are paced so they can be watched.
        let visual = call.agent && self.model.sessions.iter().any(|s| s.id == call.session && s.visual);
        cx.spawn(async move |this, cx| {
            let _awake = webview::Awake::new("doing what the agent asked in a page");
            let done = page_act(&this, &page, &native, call.act, call.agent, visual, cx).await;
            let state = state_of(&native, &page.borrow().state);
            let _ = this.update(cx, |this, cx| {
                this.page_state(call.session, state.clone());
                match call.call {
                    Some(c) => this.cmd(Cmd::UiAnswer { call: c, result: done.map(|(text, image)| serde_json::json!({"state": state, "text": text, "image": image})) }),
                    None => {
                        if let Err(e) = done {
                            this.error = Some(e);
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// a page's state, and the address bar with it when it's the open chat's.
    fn page_state(&mut self, session: SessionId, state: BrowserState) {
        let open = self.model.open.as_ref().is_some_and(|o| o.id == session);
        if open && self.browser_url_from != state.url {
            self.browser_url_from = state.url.clone();
            self.browser_url_next = Some(state.url.clone());
        }
        self.page(session).borrow_mut().state = state;
    }

    /// a few times a second while pages are native: where the shown page is,
    /// and what was picked in it.
    pub(crate) fn poll_page(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.model.open.as_ref().map(|o| o.id) else { return };
        let Some(page) = self.pages.get(&s).cloned() else { return };
        let Some(n) = page.borrow().native.clone() else { return };
        let (was, asking) = {
            let v = page.borrow();
            (v.state.clone(), v.pick_asking)
        };
        let now = state_of(&n, &was);
        if now != was {
            self.page_state(s, now.clone());
            cx.notify();
        }
        if !now.picking || asking {
            return;
        }
        page.borrow_mut().pick_asking = true;
        cx.spawn(async move |this, cx| {
            let bg = cx.background_executor().clone();
            let answer = eval(&bg, &n, webview::PICKED).await.unwrap_or_default();
            let p = &answer["picked"];
            let picked = if p.is_object() && !p["cancelled"].as_bool().unwrap_or(false) {
                let num = |k: &str| p[k].as_f64().unwrap_or(0.0);
                let pad = 6.0;
                let (x, y) = ((num("vx") - pad).max(0.0), (num("vy") - pad).max(0.0));
                let image = snapshot(&bg, &n, Some((x, y, num("width") + pad * 2.0, num("height") + pad * 2.0))).await.map(|png| base64::engine::general_purpose::STANDARD.encode(png)).unwrap_or_default();
                Some(Picked::from_described(p, image))
            } else {
                None
            };
            // pointing is over when something was picked, Escape was pressed, or the page moved on.
            let over = p.is_object() || !answer["on"].as_bool().unwrap_or(false);
            let _ = this.update(cx, |this, cx| {
                let mut v = page.borrow_mut();
                v.pick_asking = false;
                if over {
                    v.state.picking = false;
                }
                drop(v);
                if let Some(picked) = picked
                    && this.model.open.as_ref().is_some_and(|o| o.id == s)
                {
                    this.add_pick(picked);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// the page took the keyboard (a click on it, which the window doesn't
    /// see): the window's own focus follows, so its shortcuts don't act on
    /// what was focused before (pasting into the message box, say).
    pub(crate) fn follow_page_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(s) = self.model.open.as_ref().map(|o| o.id) else { return };
        let on_page = self.pages.get(&s).and_then(|p| p.borrow().native.clone()).is_some_and(|n| n.web.has_focus());
        if on_page && !self.browser_focus.is_focused(window) {
            window.focus(&self.browser_focus, cx);
        }
    }

    /// a click in the window: one on the page gives it the keyboard (and the
    /// window's shortcuts stay out of its way); one elsewhere takes it back.
    pub(crate) fn page_focus(&mut self, at: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.native_view else { return };
        let shown = self.browser_visible().then(|| self.model.open.as_ref().map(|o| o.id)).flatten();
        let on_page = shown.and_then(|s| self.pages.get(&s)).is_some_and(|p| {
            let v = p.borrow();
            v.native.is_some() && v.bounds.is_some_and(|b| b.contains(&at))
        });
        if on_page {
            window.focus(&self.browser_focus, cx);
        } else {
            webview::reclaim_focus(view);
        }
    }

    /// the Browser pane.
    pub(crate) fn browser_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else {
            return div().p_4().set(Type::Callout).text_color(p.secondary).child("Open a chat to see its browser.").into_any_element();
        };
        if let Some(engine) = self.browser_engine.as_ref().filter(|e| e.name.is_none() && self.native_view.is_none()) {
            return self.install_card(engine, cx);
        }
        let page = self.pages.get(&session).cloned().unwrap_or_default();
        let (state, activity) = {
            let v = page.borrow();
            (v.state.clone(), v.activity.clone().filter(|(_, _, t)| t.elapsed() < SAID_FOR))
        };
        let tool = |id: &'static str, icon: &'static str, tip: &'static str| Button::new(id).ghost().small().icon(Icon::empty().path(format!("icons/{icon}.svg"))).tooltip(tip);
        let url = state.url.clone();
        let toolbar = h_flex()
            .h(theme::z(40.))
            .px(theme::z(8.))
            .gap(theme::z(2.))
            .items_center()
            .border_b_1()
            .border_color(p.separator)
            .child(tool("browser-back", "arrow-left", "Back").on_click(cx.listener(|this, _, _, cx| this.act(BrowserAct::Back, cx))))
            .child(tool("browser-forward", "arrow-right", "Forward").on_click(cx.listener(|this, _, _, cx| this.act(BrowserAct::Forward, cx))))
            .child(tool("browser-reload", if state.loading { "loader" } else { "rotate-cw" }, "Reload").on_click(cx.listener(|this, _, _, cx| this.act(BrowserAct::Reload, cx))))
            .child(div().flex_1().min_w_0().px(theme::z(4.)).child(Input::new(&self.browser_url).id("browser-url").small()))
            .child(
                Button::new("browser-pick")
                    .small()
                    .when(state.picking, |b| b.primary())
                    .when(!state.picking, |b| b.ghost())
                    .icon(Icon::empty().path("icons/mouse-pointer-click.svg"))
                    .label("Pick")
                    .tooltip("Point at part of the page to put it in your message")
                    .on_click(cx.listener(move |this, _, _, cx| this.act(BrowserAct::Pick { on: !state.picking }, cx))),
            )
            .child(tool("browser-external", "arrow-up-right", "Open in your browser").on_click(move |_, _, cx| {
                if !url.is_empty() {
                    cx.open_url(&url);
                }
            }));
        let me = cx.entity().downgrade();
        let covered = self.page_covered();
        // the pane's corners, inside its border, unless panes sit flush.
        let radius = if self.ui.theme.compact.unwrap_or(false) { 0.0 } else { f64::from(theme::R_PANEL) - 1.0 };
        let view = page.clone();
        let surface = canvas(
            {
                let view = view.clone();
                move |bounds, _, _| view.borrow_mut().bounds = Some(bounds)
            },
            {
                let view = view.clone();
                move |bounds, _, window, _| {
                let mut v = view.borrow_mut();
                // a native page sits exactly here, unless something floats over it.
                // by now everything floating has been placed, so this frame knows.
                if let Some(n) = &v.native {
                    let o = bounds.origin;
                    n.web.place(f64::from(f32::from(o.x)), f64::from(f32::from(o.y)), f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height)), radius);
                    n.web.show(!covered && !float_over(&bounds));
                    return;
                }
                if let Some((bytes, w, h)) = v.next.take()
                    && let Some(buf) = image::RgbaImage::from_raw(w, h, bytes)
                {
                    let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                    if let Some((old, ..)) = v.frame.replace((fresh, w, h)) {
                        let _ = window.drop_image(old);
                    }
                }
                if let Some((img, w, h)) = v.frame.clone() {
                    // the page is as wide as the pane was when it was told its size; its
                    // frames come at any pixel density, and are drawn at that width, from
                    // the top left, so a pane mid-resize doesn't stretch them.
                    let wide = v.told.map_or(f32::from(bounds.size.width), |t| t.0 as f32);
                    let at = Bounds::new(bounds.origin, size(px(wide), px(wide * h as f32 / w.max(1) as f32)));
                    let _ = window.paint_image(bounds, at, Corners::default(), img, 0, false);
                }
                // where the agent just clicked: a ring that fades.
                if let Some((_, Some((x, y)), t)) = v.activity.clone() {
                    let age = t.elapsed().as_secs_f32();
                    if age < 1.2 {
                        let r = 10.0 + age * 18.0;
                        let c = point(bounds.origin.x + px(x as f32), bounds.origin.y + px(y as f32));
                        let mut b = PathBuilder::stroke(px(2.5));
                        for i in 0..=32 {
                            let a = std::f32::consts::TAU * i as f32 / 32.0;
                            let pt = point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
                            if i == 0 { b.move_to(pt) } else { b.line_to(pt) }
                        }
                        if let Ok(path) = b.build() {
                            window.paint_path(path, gpui_kit::hsla(0.6, 0.9, 0.6, 1.0 - age / 1.2));
                        }
                        window.request_animation_frame();
                    }
                }
            }
            },
        )
        .size_full();
        // where on the page a pointer is, in the page's own pixels.
        let local = {
            let view = view.clone();
            move |pos: Point<Pixels>| -> Option<(f64, f64)> {
                let b = view.borrow().bounds?;
                Some((f64::from(f32::from(pos.x - b.origin.x)), f64::from(f32::from(pos.y - b.origin.y))))
            }
        };
        let (down, up, moved, wheel) = (local.clone(), local.clone(), local.clone(), local);
        let viewport = div()
            .id("browser-page")
            .test_support()
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .bg(gpui_kit::white())
            // round like the page on it and the pane around it, or its square corners show.
            .rounded_b(px(radius as f32))
            .track_focus(&self.browser_focus)
            .cursor(if state.picking { CursorStyle::Crosshair } else { CursorStyle::Arrow })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                window.focus(&this.browser_focus, cx);
                if let Some((x, y)) = down(ev.position) {
                    this.stream_act(BrowserAct::Mouse { kind: MouseKind::Down, x, y, clicks: ev.click_count as u32, modifiers: 0 });
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(move |this, ev: &MouseUpEvent, _, _| {
                if let Some((x, y)) = up(ev.position) {
                    this.stream_act(BrowserAct::Mouse { kind: MouseKind::Up, x, y, clicks: ev.click_count as u32, modifiers: 0 });
                }
            }))
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, _| {
                // hovers matter (menus, a picked element's outline), but not sixty times a second.
                let now = Instant::now();
                if this.browser_moved.is_some_and(|t| now.duration_since(t) < Duration::from_millis(30)) {
                    return;
                }
                this.browser_moved = Some(now);
                if let Some((x, y)) = moved(ev.position) {
                    this.stream_act(BrowserAct::Mouse { kind: MouseKind::Move, x, y, clicks: 0, modifiers: 0 });
                }
            }))
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, window, _| {
                let d = ev.delta.pixel_delta(window.line_height());
                if let Some((x, y)) = wheel(ev.position) {
                    this.stream_act(BrowserAct::Wheel { x, y, dx: -f64::from(f32::from(d.x)), dy: -f64::from(f32::from(d.y)) });
                }
            }))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let k = &ev.keystroke;
                // the window's own shortcuts stay the window's.
                if k.modifiers.platform {
                    return;
                }
                let named = matches!(k.key.as_str(), "enter" | "tab" | "escape" | "backspace" | "delete" | "up" | "down" | "left" | "right" | "home" | "end" | "pageup" | "pagedown");
                if named {
                    this.stream_act(BrowserAct::Key { key: k.key.clone() });
                } else if let Some(text) = k.key_char.clone().filter(|t| !t.is_empty()) {
                    this.stream_act(BrowserAct::Type { text, r#ref: None, selector: None });
                } else {
                    return;
                }
                cx.stop_propagation();
            }))
            .child(surface)
            // over the page, native or not.
            .when_some(activity.map(|(what, ..)| what), |d, what| {
                d.child(deferred(
                    h_flex()
                        .absolute()
                        .top(px(10.))
                        .left(px(10.))
                        .px(px(10.))
                        .py(px(5.))
                        .gap(px(6.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(gpui_kit::black().opacity(0.72))
                        .text_color(gpui_kit::white())
                        .set(Type::Callout)
                        .child(super::live_dot(p.blocked, &p))
                        .child(format!("Agent {what}")),
                ))
            })
            .when(state.picking, |d| {
                d.child(deferred(
                    div()
                        .absolute()
                        .bottom(px(10.))
                        .left(px(10.))
                        .px(px(10.))
                        .py(px(5.))
                        .rounded(px(theme::R_CONTROL))
                        .bg(theme::info(&p))
                        .text_color(gpui_kit::white())
                        .set(Type::Callout)
                        .child("Click the part of the page you mean"),
                ))
            });
        let _ = me;
        v_flex().size_full().child(toolbar).child(viewport).into_any_element()
    }

    /// no Chromium on this Mac: what's needed, and the way to get it.
    fn install_card(&self, engine: &tessel_proto::BrowserEngine, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let busy = engine.installing.clone();
        v_flex()
            .id("browser-install")
            .test_support()
            .size_full()
            .items_center()
            .justify_center()
            .gap(theme::z(10.))
            .p_6()
            .child(Icon::empty().path("icons/globe.svg").size(theme::z(24.)).text_color(p.tertiary))
            .child(div().set(Type::Headline).text_color(p.label).child("A browser for you and the agent"))
            .child(
                div()
                    .max_w(px(380.))
                    .text_center()
                    .set(Type::Callout)
                    .text_color(p.secondary)
                    .child("Tessel runs pages in a Chromium-based browser it can drive, and none is installed. Chrome for Testing is Google's build of Chrome for programs to drive; it stays in Tessel's own folder and doesn't touch your browsers."),
            )
            .child(match busy {
                Some(what) => h_flex().gap(px(8.)).items_center().child(super::live_dot(p.blocked, &p)).child(div().set(Type::Callout).text_color(p.label).child(what)).into_any_element(),
                None => Button::new("browser-install-go")
                    .primary()
                    .label("Install Chrome for Testing (about 150 MB)")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.cmd(Cmd::Ask { tag: "browser.install", req: tessel_proto::Request::BrowserInstall });
                        if let Some(e) = this.browser_engine.as_mut() {
                            e.installing = Some("Starting".into());
                        }
                        cx.notify();
                    }))
                    .into_any_element(),
            })
            .into_any_element()
    }

    pub(crate) fn browser_answer(&mut self, result: Result<tessel_proto::Response, String>) {
        match result {
            Ok(tessel_proto::Response::BrowserEngine { engine }) => self.browser_engine = Some(engine),
            Err(e) => self.error = Some(e),
            _ => {}
        }
    }
}

/// where a native page is now.
fn state_of(n: &Native, was: &BrowserState) -> BrowserState {
    BrowserState { url: n.web.url(), title: n.web.title(), loading: n.web.loading(), picking: was.picking }
}

/// wait for a page to settle after something that may load one: up to ten seconds.
async fn settle(n: &Native, cx: &mut AsyncApp) {
    cx.background_executor().timer(Duration::from_millis(120)).await;
    for _ in 0..100 {
        if !n.web.loading() {
            return;
        }
        cx.background_executor().timer(Duration::from_millis(100)).await;
    }
}

/// how long a page has to answer before it's said not to.
const PAGE_ANSWERS_IN: Duration = Duration::from_secs(10);

/// `fut`, unless `PAGE_ANSWERS_IN` passes first.
async fn in_time<T>(bg: &BackgroundExecutor, fut: impl std::future::Future<Output = T>) -> Option<T> {
    let (mut fut, mut late) = (std::pin::pin!(fut), std::pin::pin!(bg.timer(PAGE_ANSWERS_IN)));
    std::future::poll_fn(|cx| {
        if let std::task::Poll::Ready(v) = fut.as_mut().poll(cx) {
            return std::task::Poll::Ready(Some(v));
        }
        if late.as_mut().poll(cx).is_ready() {
            return std::task::Poll::Ready(None);
        }
        std::task::Poll::Pending
    })
    .await
}

/// a script's value, with the page gone, silent or the script failing as an error.
async fn eval(bg: &BackgroundExecutor, n: &Native, js: &str) -> Result<serde_json::Value, String> {
    match in_time(bg, n.web.eval(js)).await {
        Some(Ok(answer)) => answer,
        Some(Err(_)) => Err("the page went away".into()),
        None => Err("the page didn't answer in time".into()),
    }
}

/// a picture of the page, or of part of it.
async fn snapshot(bg: &BackgroundExecutor, n: &Native, rect: Option<(f64, f64, f64, f64)>) -> Result<Vec<u8>, String> {
    match in_time(bg, n.web.snapshot(rect)).await {
        Some(Ok(png)) => png,
        Some(Err(_)) => Err("the page went away".into()),
        None => Err("the page didn't make a picture in time".into()),
    }
}

/// a script that answers true, or says what went wrong.
async fn run(bg: &BackgroundExecutor, n: &Native, js: &str) -> Result<(), String> {
    match eval(bg, n, js).await? {
        serde_json::Value::String(e) => Err(e),
        _ => Ok(()),
    }
}

/// where an element (by ref or selector) is, scrolled into view, and what it says.
async fn locate(bg: &BackgroundExecutor, n: &Native, r: Option<&str>, selector: Option<&str>) -> Result<Option<(f64, f64, String)>, String> {
    let Some(js) = tessel_proto::locate_target(r, selector) else { return Ok(None) };
    let at = eval(bg, n, &js).await?;
    if at.is_null() {
        return Err(format!("there's no {} on the page", tessel_proto::target_name(r, selector)));
    }
    Ok(Some((at["x"].as_f64().unwrap_or(0.0), at["y"].as_f64().unwrap_or(0.0), at["label"].as_str().unwrap_or_default().to_owned())))
}

/// one act, done in the page. what reading it found, and a picture, if asked for.
async fn page_act(
    this: &WeakEntity<Shell>,
    page: &Rc<RefCell<PageView>>,
    n: &Native,
    act: BrowserAct,
    agent: bool,
    visual: bool,
    cx: &mut AsyncApp,
) -> Result<(Option<String>, Option<String>), String> {
    // what the agent does is said over the page, and where, with a ring.
    let tell = |what: String, at: Option<(f64, f64)>, cx: &mut AsyncApp| {
        if agent {
            page.borrow_mut().activity = Some((what, at, Instant::now()));
            let _ = this.update(cx, |_, cx| cx.notify());
        }
    };
    // where it acts: in visual mode the cursor glides there first and what's there is
    // outlined; either way, a ring.
    let ring = async |bg: &BackgroundExecutor, at: (f64, f64)| {
        if visual {
            let _ = eval(bg, n, &webview::point_at(at.0, at.1)).await;
            bg.timer(Duration::from_millis(380)).await;
        }
        if agent {
            let _ = eval(bg, n, &webview::ring(at.0, at.1)).await;
        }
    };
    let bg = cx.background_executor().clone();
    let (mut text, mut image) = (None, None);
    match act {
        BrowserAct::Open { url: None } => settle(n, cx).await,
        BrowserAct::Open { url: Some(url) } | BrowserAct::Navigate { url } => {
            let url = url_of(&url);
            n.web.load(&url)?;
            tell(format!("went to {url}"), None, cx);
            settle(n, cx).await;
        }
        BrowserAct::Back => {
            n.web.back();
            tell("went back".into(), None, cx);
            settle(n, cx).await;
        }
        BrowserAct::Forward => {
            n.web.forward();
            tell("went forward".into(), None, cx);
            settle(n, cx).await;
        }
        BrowserAct::Reload => {
            n.web.reload();
            tell("reloaded".into(), None, cx);
            settle(n, cx).await;
        }
        BrowserAct::Click { r#ref, selector, x, y } => {
            let (x, y, label) = match (locate(&bg, n, r#ref.as_deref(), selector.as_deref()).await?, x, y) {
                (Some(at), _, _) => at,
                (None, Some(x), Some(y)) => (x, y, format!("at {x:.0}, {y:.0}")),
                _ => return Err("click needs a ref, a selector, or x and y".into()),
            };
            tell(format!("clicked {label}"), Some((x, y)), cx);
            ring(&bg, (x, y)).await;
            run(&bg, n, &webview::click(x, y)).await?;
            // a click may start loading a page.
            settle(n, cx).await;
        }
        BrowserAct::Type { text: t, r#ref, selector } => {
            match locate(&bg, n, r#ref.as_deref(), selector.as_deref()).await? {
                Some((x, y, label)) => {
                    tell(format!("typed into {label}"), Some((x, y)), cx);
                    ring(&bg, (x, y)).await;
                    run(&bg, n, &webview::click(x, y)).await?;
                }
                None => tell("typed".into(), None, cx),
            }
            if visual {
                // a letter at a time, as far as is worth watching; then the rest.
                let chars: Vec<char> = t.chars().collect();
                let (shown, rest) = chars.split_at(chars.len().min(120));
                for c in shown {
                    run(&bg, n, &webview::type_text(&c.to_string())).await?;
                    bg.timer(Duration::from_millis(28)).await;
                }
                if !rest.is_empty() {
                    run(&bg, n, &webview::type_text(&rest.iter().collect::<String>())).await?;
                }
            } else {
                run(&bg, n, &webview::type_text(&t)).await?;
            }
        }
        BrowserAct::Select { value, r#ref, selector } => {
            let js = tessel_proto::select_script(r#ref.as_deref(), selector.as_deref(), &value).ok_or("select needs a ref or a selector")?;
            if visual && let Ok(Some((x, y, _))) = locate(&bg, n, r#ref.as_deref(), selector.as_deref()).await {
                ring(&bg, (x, y)).await;
            }
            run(&bg, n, &js).await?;
            tell(format!("chose {value}"), None, cx);
        }
        BrowserAct::Snapshot { root } => {
            tell("read the page".into(), None, cx);
            text = Some(eval(&bg, n, &tessel_proto::snapshot_script(root.as_deref())).await?.as_str().unwrap_or_default().to_owned());
        }
        BrowserAct::Key { key } => {
            run(&bg, n, &webview::key(&key)).await?;
            let (name, ..) = tessel_proto::key_codes(&key);
            tell(format!("pressed {}", if name.is_empty() { key.as_str() } else { name }), None, cx);
            settle(n, cx).await;
        }
        BrowserAct::Scroll { dx, dy, x, y } => {
            run(&bg, n, &webview::scroll_as(x, y, dx, dy, visual)).await?;
            tell("scrolled".into(), None, cx);
            if visual {
                bg.timer(Duration::from_millis(450)).await;
            }
        }
        BrowserAct::Read => {
            tell("read the page".into(), None, cx);
            text = Some(eval(&bg, n, &read_script()).await?.as_str().unwrap_or_default().to_owned());
        }
        BrowserAct::Screenshot => {
            tell("looked at the page".into(), None, cx);
            let png = snapshot(&bg, n, None).await?;
            image = Some(base64::engine::general_purpose::STANDARD.encode(png));
        }
        BrowserAct::Pick { on } => {
            eval(&bg, n, &if on { webview::pick_on() } else { webview::PICK_OFF.to_owned() }).await?;
            page.borrow_mut().state.picking = on;
        }
        // the pane's own: a native page takes these directly.
        BrowserAct::Watch { .. } | BrowserAct::Resize { .. } | BrowserAct::Mouse { .. } | BrowserAct::Wheel { .. } => {}
    }
    Ok((text, image))
}

/// the window's view, when pages can be shown natively in it: on the Mac, in
/// a real window, unless `TESSEL_BROWSER_STREAM` asks for the daemon's browser.
pub(crate) fn native_host(window: &Window) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if std::env::var_os("TESSEL_BROWSER_STREAM").is_some() {
            return None;
        }
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr() as usize),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

/// a window's own view, as AppKit has it: what its number, its first
/// responder and pages in it are found from.
pub(crate) fn view_of(window: &Window) -> Option<usize> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => Some(h.ns_view.as_ptr() as usize),
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}
