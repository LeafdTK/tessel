//! the Browser pane: the open chat's page, live. it's the page the agent
//! drives with its tools, so what it does shows here as it does it (a label
//! saying what, a ring where it clicked), and you can use the page too: click,
//! scroll, type. Pick points at a part of the page and puts it in your next
//! message: a picture of it, its selector and its HTML, for you to say what
//! should change.
//!
//! the daemon runs the browser and sends frames while a pane watches; this
//! only shows them and passes your input back.

use super::*;
use gpui_kit::component::Icon;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_proto::{BrowserAct, BrowserState, MouseKind, Picked};

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
                // the agent is using the browser: let it be seen.
                if self.model.open.as_ref().is_some_and(|o| o.id == session) && !self.browser_visible() {
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

    /// what you pointed at, into the message you're writing: its picture as an
    /// attachment, where it is in words.
    fn add_pick(&mut self, p: Picked) {
        let html: String = p.html.chars().take(800).collect();
        let note = format!("\n\nIn the browser, on {}: `{}`\n```html\n{}\n```\n", p.url, p.selector, html.trim());
        self.draft_append = Some(note);
        if !p.image.is_empty() {
            self.attachments.push(tessel_proto::Attachment { name: format!("{}.png", p.selector.chars().filter(|c| c.is_alphanumeric()).take(24).collect::<String>()), media_type: "image/png".into(), data: p.image });
        }
        self.focus_composer_next = true;
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

    fn act(&mut self, act: BrowserAct) {
        if let Some(s) = self.model.open.as_ref().map(|o| o.id) {
            self.cmd(Cmd::Browser { session: s, act });
        }
    }

    /// the Browser pane.
    pub(crate) fn browser_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else {
            return div().p_4().set(Type::Callout).text_color(p.secondary).child("Open a chat to see its browser.").into_any_element();
        };
        if let Some(engine) = self.browser_engine.as_ref().filter(|e| e.name.is_none()) {
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
            .child(tool("browser-back", "arrow-left", "Back").on_click(cx.listener(|this, _, _, _| this.act(BrowserAct::Back))))
            .child(tool("browser-forward", "arrow-right", "Forward").on_click(cx.listener(|this, _, _, _| this.act(BrowserAct::Forward))))
            .child(tool("browser-reload", if state.loading { "loader" } else { "rotate-cw" }, "Reload").on_click(cx.listener(|this, _, _, _| this.act(BrowserAct::Reload))))
            .child(div().flex_1().min_w_0().px(theme::z(4.)).child(Input::new(&self.browser_url).id("browser-url").small()))
            .child(
                Button::new("browser-pick")
                    .small()
                    .when(state.picking, |b| b.primary())
                    .when(!state.picking, |b| b.ghost())
                    .icon(Icon::empty().path("icons/mouse-pointer-click.svg"))
                    .label("Pick")
                    .tooltip("Point at part of the page to put it in your message")
                    .on_click(cx.listener(move |this, _, _, _| this.act(BrowserAct::Pick { on: !state.picking }))),
            )
            .child(tool("browser-external", "arrow-up-right", "Open in your browser").on_click(move |_, _, cx| {
                if !url.is_empty() {
                    cx.open_url(&url);
                }
            }));
        let me = cx.entity().downgrade();
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
                if let Some((bytes, w, h)) = v.next.take()
                    && let Some(buf) = image::RgbaImage::from_raw(w, h, bytes)
                {
                    let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                    if let Some((old, ..)) = v.frame.replace((fresh, w, h)) {
                        let _ = window.drop_image(old);
                    }
                }
                if let Some((img, w, h)) = v.frame.clone() {
                    // drawn at the screen's own pixels, from the top left, as the page lays out.
                    let scale = window.scale_factor();
                    let at = Bounds::new(bounds.origin, size(px(w as f32 / scale), px(h as f32 / scale)));
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
            .track_focus(&self.browser_focus)
            .cursor(if state.picking { CursorStyle::Crosshair } else { CursorStyle::Arrow })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                window.focus(&this.browser_focus, cx);
                if let Some((x, y)) = down(ev.position) {
                    this.act(BrowserAct::Mouse { kind: MouseKind::Down, x, y, clicks: ev.click_count as u32, modifiers: 0 });
                }
            }))
            .on_mouse_up(MouseButton::Left, cx.listener(move |this, ev: &MouseUpEvent, _, _| {
                if let Some((x, y)) = up(ev.position) {
                    this.act(BrowserAct::Mouse { kind: MouseKind::Up, x, y, clicks: ev.click_count as u32, modifiers: 0 });
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
                    this.act(BrowserAct::Mouse { kind: MouseKind::Move, x, y, clicks: 0, modifiers: 0 });
                }
            }))
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, window, _| {
                let d = ev.delta.pixel_delta(window.line_height());
                if let Some((x, y)) = wheel(ev.position) {
                    this.act(BrowserAct::Wheel { x, y, dx: -f64::from(f32::from(d.x)), dy: -f64::from(f32::from(d.y)) });
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
                    this.act(BrowserAct::Key { key: k.key.clone() });
                } else if let Some(text) = k.key_char.clone().filter(|t| !t.is_empty()) {
                    this.act(BrowserAct::Type { text, selector: None });
                } else {
                    return;
                }
                cx.stop_propagation();
            }))
            .child(surface)
            .when_some(activity.map(|(what, ..)| what), |d, what| {
                d.child(
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
                )
            })
            .when(state.picking, |d| {
                d.child(
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
                )
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
