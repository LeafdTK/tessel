//! setting up a dev box, as a flow of its own: where it is, what of yours it
//! gets (your Claude: its sign-in, CLAUDE.md, settings, skills and the rest;
//! who you are to git; your GitHub sign-in), then setting it up as you
//! watch, then ready. drawn like tessel's landing page: a dithered meadow at
//! dusk on the left, the steps on the right.
//!
//! it also opens at "what it gets" for a box that's set up already, to bring
//! things later.

use super::*;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::switch::Switch;
use tessel_remote::carry::{Carry, Inventory};
use tessel_remote::claude_login::{Heard, Login};
use tessel_remote::hosts::{Host, Hosts};
use tessel_remote::setup::{Options, Said};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Where,
    Bring,
    Setup,
    Ready,
}

impl Step {
    fn number(self) -> usize {
        match self {
            Step::Where => 0,
            Step::Bring => 1,
            Step::Setup => 2,
            Step::Ready => 3,
        }
    }
}

/// where signing Claude in there stands.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum SignIn {
    /// not asked for: they'll sign in there themselves (or already have).
    Later,
    /// `claude setup-token` is running: waiting on the browser.
    Waiting { url: Option<String>, wants_code: bool },
    Done,
    Failed(String),
}

pub(crate) struct Flow {
    pub(crate) step: Step,
    /// bringing things to a box that's set up already: no "where", no setup.
    pub(crate) existing: Option<String>,
    pub(crate) inv: Inventory,
    pub(crate) carry: Carry,
    pub(crate) sign_in: SignIn,
    login: Option<Login>,
    code: Entity<InputState>,
    /// the box being added, once "where" is filled in (and its udp port).
    pending: Option<(Host, u16)>,
    /// the box it ended up as.
    pub(crate) host: Option<String>,
    pub(crate) error: Option<String>,
    opened: Instant,
    /// the meadow, the frosted pane over it, and where the meadow is (the pane needs it).
    art: backdrop::Shared,
    frost: backdrop::Shared,
    sheet: std::rc::Rc<std::cell::Cell<Option<Bounds<Pixels>>>>,
    /// bringing to an existing box: how it went (the setup run says it for a new one).
    bring_run: Option<super::hosts::SetupRun>,
}

impl Shell {
    pub(crate) fn flow_open(&self) -> bool {
        self.hosts.flow.is_some()
    }

    /// start setting up a new box.
    pub(crate) fn setup_flow_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_flow(None, window, cx);
    }

    /// bring things to `host`, set up already.
    pub(crate) fn setup_flow_bring(&mut self, host: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_flow(Some(host), window, cx);
    }

    fn open_flow(&mut self, existing: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let inv = tessel_remote::carry::inventory();
        let carry = Carry::suggested(&inv);
        let code = cx.new(|cx| InputState::new(window, cx).placeholder("Paste the code the page shows"));
        self.close_settings(cx);
        self.hosts.adding = existing.is_none();
        self.hosts.form = None;
        self.hosts.flow = Some(Flow {
            step: if existing.is_some() { Step::Bring } else { Step::Where },
            host: existing.clone(),
            existing,
            inv,
            carry,
            sign_in: SignIn::Later,
            login: None,
            code,
            pending: None,
            error: None,
            opened: Instant::now(),
            art: Default::default(),
            frost: Default::default(),
            sheet: Default::default(),
            bring_run: None,
        });
        // the meadow drifts while it's open: a frame every half second.
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(500)).await;
                let open = in_window(&this, cx, |this, _, cx| {
                    let open = this.hosts.flow.is_some();
                    if open {
                        cx.notify();
                    }
                    open
                });
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn flow_close(&mut self, cx: &mut Context<Self>) {
        // setting up carries on without the flow; only its window goes.
        self.hosts.flow = None;
        self.hosts.adding = false;
        self.hosts.form = None;
        cx.notify();
    }

    fn flow_next(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(step) = self.hosts.flow.as_ref().map(|f| f.step) else { return };
        match step {
            Step::Where => match self.form_host(cx) {
                Ok(pending) => {
                    if let Some(f) = self.hosts.flow.as_mut() {
                        f.pending = Some(pending);
                        f.step = Step::Bring;
                        f.error = None;
                    }
                }
                Err(e) => {
                    if let Some(f) = self.hosts.flow.as_mut() {
                        f.error = Some(e);
                    }
                }
            },
            Step::Bring => self.flow_go(window, cx),
            Step::Setup => {}
            Step::Ready => self.flow_close(cx),
        }
        cx.notify();
    }

    fn flow_back(&mut self, cx: &mut Context<Self>) {
        if let Some(f) = self.hosts.flow.as_mut()
            && f.step == Step::Bring
            && f.existing.is_none()
        {
            f.step = Step::Where;
            f.error = None;
        }
        cx.notify();
    }

    /// set it up (or bring things to it), with what was picked.
    fn flow_go(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.hosts.flow.as_mut() else { return };
        if matches!(f.sign_in, SignIn::Waiting { .. }) {
            f.error = Some("Finish signing in first (or choose to sign in there later).".into());
            return;
        }
        let carry = f.carry.clone();
        f.error = None;
        f.step = Step::Setup;
        if let Some(id) = f.existing.clone() {
            self.flow_bring(id, carry, window, cx);
            return;
        }
        let Some((host, port)) = f.pending.clone() else { return };
        let mut list = Hosts::load(&self.hosts.home);
        let id = list.add(host);
        if let Err(e) = list.save(&self.hosts.home) {
            if let Some(f) = self.hosts.flow.as_mut() {
                f.error = Some(format!("Couldn't save the host: {e}"));
                f.step = Step::Bring;
            }
            return;
        }
        if let Some(f) = self.hosts.flow.as_mut() {
            f.host = Some(id.clone());
        }
        self.hosts_reload();
        self.host_setup(id, Options { port, claude: true, carry: Some(carry) }, false, window, cx);
    }

    /// bring things to a box that's set up, off the ui thread, as the flow watches.
    fn flow_bring(&mut self, id: String, carry: Carry, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = Hosts::load(&self.hosts.home).get(&id).cloned() else { return };
        let home = self.hosts.home.clone();
        if let Some(f) = self.hosts.flow.as_mut() {
            f.bring_run = Some(super::hosts::SetupRun { host: id.clone(), steps: Vec::new(), done: None, switch_after: false, started: Instant::now() });
        }
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Result<Said, Result<(), String>>>();
        std::thread::spawn(move || {
            let r = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|rt| {
                let said = tx.clone();
                let mut say = move |s: Said| {
                    let _ = said.send(Ok(s));
                };
                rt.block_on(tessel_remote::setup::bring(&home, &host, carry, &mut say)).map_err(|e| format!("{e:#}"))
            });
            let _ = tx.send(Err(r));
        });
        cx.spawn_in(window, async move |this, cx| {
            while let Some(m) = rx.recv().await {
                let alive = in_window(&this, cx, |this, _, cx| {
                    if let Some(run) = this.hosts.flow.as_mut().and_then(|f| f.bring_run.as_mut()) {
                        match m {
                            Ok(Said::Step(s)) => run.steps.push((s, Vec::new())),
                            Ok(Said::Note(n)) => match run.steps.last_mut() {
                                Some((_, notes)) => notes.push(n),
                                None => run.steps.push((n, Vec::new())),
                            },
                            Err(r) => run.done = Some(r),
                        }
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// sign Claude in for the box: a token made here, in a terminal of its own.
    fn flow_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(f) = self.hosts.flow.as_mut() else { return };
        let mut login = match tessel_remote::claude_login::start() {
            Ok(l) => l,
            Err(e) => {
                f.sign_in = SignIn::Failed(format!("{e:#}"));
                cx.notify();
                return;
            }
        };
        let mut heard = std::mem::replace(&mut login.heard, tokio::sync::mpsc::unbounded_channel().1);
        f.login = Some(login);
        f.sign_in = SignIn::Waiting { url: None, wants_code: false };
        cx.spawn_in(window, async move |this, cx| {
            while let Some(h) = heard.recv().await {
                let alive = in_window(&this, cx, |this, window, cx| {
                    let Some(f) = this.hosts.flow.as_mut() else { return };
                    match h {
                        Heard::Url(u) => {
                            cx.open_url(&u);
                            f.sign_in = SignIn::Waiting { url: Some(u), wants_code: matches!(f.sign_in, SignIn::Waiting { wants_code: true, .. }) };
                        }
                        Heard::WantsCode => {
                            let url = match &f.sign_in {
                                SignIn::Waiting { url, .. } => url.clone(),
                                _ => None,
                            };
                            f.sign_in = SignIn::Waiting { url, wants_code: true };
                            f.code.update(cx, |s, cx| s.focus(window, cx));
                        }
                        Heard::Token(t) => {
                            f.carry.claude_token = Some(t);
                            f.sign_in = SignIn::Done;
                            f.login = None;
                        }
                        Heard::Failed(why) => {
                            f.sign_in = SignIn::Failed(why);
                            f.login = None;
                        }
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn flow_send_code(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.hosts.flow.as_mut() else { return };
        let code = f.code.read(cx).value().trim().to_owned();
        if code.is_empty() {
            return;
        }
        if let Some(l) = &f.login
            && let Err(e) = l.code(&code)
        {
            f.sign_in = SignIn::Failed(e.to_string());
        }
        cx.notify();
    }

    /// the setup run the flow is watching: its own (bringing), or the box's setup.
    fn flow_run(&self) -> Option<&super::hosts::SetupRun> {
        let f = self.hosts.flow.as_ref()?;
        f.bring_run.as_ref().or_else(|| self.hosts.run.as_ref().filter(|r| Some(&r.host) == f.host.as_ref()))
    }

    // ---- drawing ------------------------------------------------------------------
    // the landing page's look, whatever the window's theme: a meadow at dusk
    // filling the sheet, and on it a pane of frosted glass with the steps.

    pub(crate) fn setup_flow_layer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(f) = self.hosts.flow.as_ref() else { return div().into_any_element() };
        // finished setting up: ready, whatever step it was on.
        let step = match (f.step, self.flow_run().and_then(|r| r.done.as_ref())) {
            (Step::Setup, Some(Ok(()))) => Step::Ready,
            (s, _) => s,
        };
        let frame = (f.opened.elapsed().as_millis() / 500) as u32;
        let sheet = f.sheet.clone();
        let art = backdrop::image_el_at(f.art.clone(), cx.entity().downgrade(), Corners::all(px(22.)), move |b, scale| {
            sheet.set(Some(b));
            Some(meadow_key(b, b, scale, frame, false))
        });
        let sheet = f.sheet.clone();
        let frost = backdrop::image_el_at(f.frost.clone(), cx.entity().downgrade(), Corners::all(px(20.)), move |b, scale| {
            let field = sheet.get()?;
            Some(meadow_key(b, field, scale, frame, true))
        });
        let name = f.pending.as_ref().map(|(h, _)| h.name.clone()).or_else(|| f.host.as_ref().and_then(|id| self.hosts.list.get(id)).map(|h| h.name.clone()));
        let sandbox = f.pending.as_ref().map(|(h, _)| h.vercel.is_some()).or_else(|| f.host.as_ref().and_then(|id| self.hosts.list.get(id)).map(|h| h.vercel.is_some()));
        let (title, about) = match step {
            Step::Where => ("Add a dev box".to_owned(), "A machine of your own, somewhere else. Chats, terminals and git run there; this Mac shows them."),
            Step::Bring => ("Bring your Claude".to_owned(), "Your sign-in, instructions and settings, as you've made them. Each one is yours to bring or leave."),
            Step::Setup => (format!("Setting up {}", name.clone().unwrap_or_default()), "Closing this doesn't stop it."),
            Step::Ready => (format!("{} is ready", name.clone().unwrap_or_default()), "Show it from the card at the top of the sidebar, any time."),
        };

        // the steps, as tabs.
        let mut tabs = h_flex().gap(px(4.));
        for (i, n) in ["Where", "Bring", "Set up", "Ready"].iter().enumerate() {
            if f.existing.is_some() && i == 0 {
                continue;
            }
            let on = i == step.number();
            tabs = tabs.child(div().px(px(12.)).py(px(6.)).rounded(px(10.)).text_size(px(13.)).when(on, |d| d.bg(BONE.opacity(0.07)).text_color(BONE)).when(!on, |d| d.text_color(if i < step.number() { MINT } else { ASH })).child(*n));
        }

        let body: AnyElement = match step {
            Step::Where => self.flow_where(cx),
            Step::Bring => self.flow_bring_el(cx),
            Step::Setup | Step::Ready => self.flow_progress(),
        };

        let running = step == Step::Setup && self.flow_run().is_some_and(|r| r.done.is_none());
        let failed = self.flow_run().and_then(|r| r.done.as_ref()).is_some_and(Result::is_err);
        let next_label = match step {
            Step::Where => "Next",
            Step::Bring if f.existing.is_some() => "Bring it",
            Step::Bring => "Set it up",
            Step::Setup if failed => "Close",
            Step::Setup => "Working…",
            Step::Ready => "Done",
        };
        let footer = h_flex()
            .gap(px(8.))
            .pt(px(18.))
            .children(f.error.clone().map(|e| div().flex_1().min_w_0().text_size(px(13.)).text_color(AMBER).child(e)))
            .when(f.error.is_none(), |d| d.child(div().flex_1()))
            .when(step == Step::Bring && f.existing.is_none(), |d| d.child(chip("flow-back", "Back", false).on_click(cx.listener(|this, _, _, cx| this.flow_back(cx)))))
            .when(step == Step::Ready, |d| {
                let id = f.host.clone();
                d.child(chip("flow-show", "Show it", false).on_click(cx.listener(move |this, _, _, cx| {
                    let id = id.clone();
                    this.flow_close(cx);
                    this.switch_host(id, cx);
                })))
            })
            .when(!running, |d| {
                d.child(chip("flow-next", next_label, true).on_click(cx.listener(move |this, _, window, cx| {
                    if failed {
                        this.flow_close(cx);
                    } else {
                        this.flow_next(window, cx);
                    }
                })))
            });

        let card = v_flex()
            .relative()
            .w(px(560.))
            .max_w(relative(0.9))
            .h(px(540.))
            .max_h(relative(0.88))
            .rounded(px(20.))
            .border_1()
            .border_color(BONE.opacity(0.09))
            .shadow_lg()
            .child(div().absolute().inset_0().child(frost))
            // light catches the top edge of the glass.
            .child(div().absolute().inset_0().rounded(px(20.)).bg(linear_gradient(180., linear_color_stop(BONE.opacity(0.06), 0.0), linear_color_stop(BONE.opacity(0.0), 0.3))))
            .child(
                v_flex()
                    .relative()
                    .size_full()
                    .p(px(28.))
                    .child(h_flex().child(tabs).child(div().flex_1()).child(
                        div().id("flow-close").test_support().p(px(6.)).rounded(px(8.)).hover(|d| d.bg(BONE.opacity(0.07))).on_click(cx.listener(|this, _, _, cx| this.flow_close(cx))).child(Icon::empty().path("icons/x.svg").size(px(15.)).text_color(STONE)),
                    ))
                    .child(
                        h_flex()
                            .pt(px(22.))
                            .gap(px(14.))
                            .items_center()
                            .child(super::avatar::machine(&name.clone().unwrap_or_else(|| "new".into()), if sandbox == Some(true) { "cloud" } else { "server" }, 40., 10.))
                            .child(v_flex().flex_1().min_w_0().gap(px(3.)).child(div().text_size(px(24.)).line_height(px(28.)).font_weight(FontWeight(640.)).text_color(BONE).child(title)).child(div().text_size(px(13.5)).text_color(STONE).child(about))),
                    )
                    .child(v_flex().id("flow-body").pt(px(22.)).flex_1().min_h_0().overflow_y_scroll().child(body))
                    .child(footer),
            );

        div()
            .id("setup-flow-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(0.45))
            .flex()
            .items_center()
            .justify_center()
            .on_click(cx.listener(|this, _, _, cx| {
                if this.flow_run().is_none_or(|r| r.done.is_some()) {
                    this.flow_close(cx);
                }
            }))
            .child(
                div()
                    .id("setup-flow")
                    .test_support()
                    .aria_label("Set up a dev box")
                    .relative()
                    .w(px(880.))
                    .max_w(relative(0.94))
                    .h(px(640.))
                    .max_h(relative(0.92))
                    .rounded(px(22.))
                    .border_1()
                    .border_color(BONE.opacity(0.08))
                    .shadow_lg()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(div().absolute().inset_0().child(art))
                    .child(card),
            )
            .into_any_element()
    }

    /// where: a machine over ssh, or a vercel sandbox, and how to reach it.
    fn flow_where(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(form) = self.hosts.form.as_ref() else { return div().into_any_element() };
        let card = |id: &'static str, icon: &'static str, name: &'static str, about: &'static str, on: bool, vercel: bool, cx: &mut Context<Self>| {
            v_flex()
                .id(id)
                .test_support()
                .flex_1()
                .min_w_0()
                .p(px(14.))
                .gap(px(6.))
                .rounded(px(14.))
                .border_1()
                .border_color(if on { BONE.opacity(0.32) } else { BONE.opacity(0.08) })
                .bg(if on { BONE.opacity(0.07) } else { gpui_kit::black().opacity(0.18) })
                .when(!on, |d| d.hover(|d| d.bg(BONE.opacity(0.04))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(f) = this.hosts.form.as_mut() {
                        f.set_vercel(vercel);
                    }
                    cx.notify();
                }))
                .child(Icon::empty().path(format!("icons/{icon}.svg")).size(px(17.)).text_color(if on { BONE } else { STONE }))
                .child(div().text_size(px(14.)).font_weight(FontWeight::SEMIBOLD).text_color(BONE).child(name))
                .child(div().text_size(px(12.5)).line_height(px(17.)).text_color(STONE).child(about))
        };
        let vercel = form.is_vercel();
        v_flex()
            .gap(px(16.))
            .child(
                h_flex()
                    .gap(px(10.))
                    .items_stretch()
                    .child(card("flow-kind-ssh", "server", "A machine over ssh", "A cloud VM, a box at home: anything you can ssh into.", !vercel, false, cx))
                    .child(card("flow-kind-vercel", "cloud", "A Vercel sandbox", "Runs while you look at it, and keeps its disk.", vercel, true, cx)),
            )
            .child(self.flow_fields(cx))
            .into_any_element()
    }

    /// the fields for the kind picked, on the glass.
    fn flow_fields(&self, cx: &mut Context<Self>) -> Div {
        let Some(f) = self.hosts.form.as_ref() else { return v_flex() };
        let field = |label: &'static str, hint: &'static str, input: &Entity<InputState>, id: &'static str| {
            v_flex()
                .gap(px(6.))
                .child(div().text_size(px(12.5)).text_color(STONE).child(label))
                .child(div().id(id).test_support().rounded(px(12.)).border_1().border_color(BONE.opacity(0.09)).bg(gpui_kit::black().opacity(0.28)).px(px(12.)).py(px(8.)).text_color(BONE).child(Input::new(input).appearance(false).text_color(BONE)))
                .when(!hint.is_empty(), |d| d.child(div().text_size(px(12.)).text_color(ASH).child(hint)))
        };
        if f.is_vercel() {
            let vcpus = f.vcpus();
            let size = h_flex().gap(px(4.)).children([2u32, 4, 8].map(|n| {
                let on = n == vcpus;
                div()
                    .id(SharedString::from(format!("flow-vcpus-{n}")))
                    .test_support()
                    .px(px(12.))
                    .py(px(7.))
                    .rounded(px(10.))
                    .text_size(px(13.))
                    .when(on, |d| d.bg(BONE.opacity(0.1)).text_color(BONE))
                    .when(!on, |d| d.text_color(STONE).hover(|d| d.bg(BONE.opacity(0.05))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(f) = this.hosts.form.as_mut() {
                            f.set_vcpus(n);
                        }
                        cx.notify();
                    }))
                    .child(format!("{n} CPUs"))
            }));
            v_flex()
                .gap(px(14.))
                .child(h_flex().gap(px(12.)).items_end().child(div().flex_1().child(field("Name", "", &f.name, "host-form-name"))).child(v_flex().gap(px(6.)).child(div().text_size(px(12.5)).text_color(STONE).child("Size")).child(size)))
                .child(if f.has_token() {
                    div().text_size(px(12.5)).text_color(STONE).child("Your Vercel token is in this Mac's keychain.").into_any_element()
                } else {
                    field("Vercel token", "vercel.com › Account Settings › Tokens. Kept in this Mac's keychain.", f.token_input(), "host-form-token").into_any_element()
                })
                .child(h_flex().gap(px(12.)).child(div().flex_1().child(field("Team", "", f.team_input(), "host-form-team"))).child(div().flex_1().child(field("Project", "", f.project_input(), "host-form-project"))))
        } else {
            v_flex()
                .gap(px(14.))
                .child(field("ssh", "Your keys and ~/.ssh/config are used. There's no password prompt.", &f.ssh, "host-form-ssh"))
                .child(h_flex().gap(px(12.)).child(div().flex_1().child(field("Name", "", &f.name, "host-form-name"))).child(div().flex_1().child(field("ssh key (if not the usual)", "", &f.key, "host-form-key"))))
        }
    }

    /// what it gets: each thing of yours, on or off, and Claude's sign-in.
    fn flow_bring_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(f) = self.hosts.flow.as_ref() else { return div().into_any_element() };
        let inv = &f.inv;
        let row = |id: &'static str, name: &'static str, about: String, on: bool, have: bool, flip: fn(&mut Carry, bool), cx: &mut Context<Self>| {
            h_flex()
                .py(px(10.))
                .gap(px(12.))
                .border_b_1()
                .border_color(BONE.opacity(0.07))
                .when(!have, |d| d.opacity(0.4))
                .child(v_flex().flex_1().min_w_0().gap(px(2.)).child(div().text_size(px(14.)).text_color(BONE).child(name)).child(div().text_size(px(12.5)).text_color(STONE).child(about)))
                .child(Switch::new(id).checked(on && have).disabled(!have).on_click(cx.listener(move |this, on: &bool, _, cx| {
                    if let Some(f) = this.hosts.flow.as_mut() {
                        flip(&mut f.carry, *on);
                    }
                    cx.notify();
                })))
        };
        let kb = |b: u64| if b >= 1024 { format!("{} KB", b / 1024) } else { format!("{b} bytes") };
        let plural = |n: usize, one: &str| if n == 1 { format!("1 {one}") } else { format!("{n} {one}s") };
        let c = &f.carry;
        let sign_in: AnyElement = match &f.sign_in {
            SignIn::Later => h_flex()
                .gap(px(12.))
                .child(v_flex().flex_1().min_w_0().gap(px(2.)).child(div().text_size(px(14.)).text_color(BONE).child("Claude's sign-in")).child(div().text_size(px(12.5)).text_color(STONE).child(if inv.claude { "A token just for this box. Your Mac's own sign-in stays here: two machines on one sign-in log each other out." } else { "Claude Code isn't on this Mac: sign in on the box once it's set up." })))
                .when(inv.claude, |d| d.child(chip("flow-sign-in", "Sign in", false).on_click(cx.listener(|this, _, window, cx| this.flow_sign_in(window, cx)))))
                .into_any_element(),
            SignIn::Waiting { wants_code, .. } => v_flex()
                .gap(px(10.))
                .child(div().text_size(px(13.)).text_color(STONE).child(if *wants_code { "Approve it in your browser, then paste the code it shows." } else { "Opening your browser…" }))
                .when(*wants_code, |d| {
                    d.child(
                        h_flex()
                            .gap(px(8.))
                            .child(div().id("flow-code").test_support().flex_1().rounded(px(12.)).border_1().border_color(BONE.opacity(0.09)).bg(gpui_kit::black().opacity(0.28)).px(px(12.)).py(px(8.)).child(Input::new(&f.code).appearance(false).text_color(BONE)))
                            .child(chip("flow-code-go", "Continue", true).on_click(cx.listener(|this, _, _, cx| this.flow_send_code(cx)))),
                    )
                })
                .into_any_element(),
            SignIn::Done => h_flex().gap(px(8.)).child(Icon::empty().path("icons/circle-check.svg").size(px(15.)).text_color(MINT)).child(div().text_size(px(13.5)).text_color(MINT).child("Signed in: a token for this box, good for a year.")).into_any_element(),
            SignIn::Failed(why) => h_flex()
                .gap(px(12.))
                .child(div().flex_1().min_w_0().text_size(px(13.)).text_color(AMBER).child(format!("Didn't sign in: {why}")))
                .child(chip("flow-sign-in", "Try again", false).on_click(cx.listener(|this, _, window, cx| this.flow_sign_in(window, cx))))
                .into_any_element(),
        };
        v_flex()
            .child(div().pb(px(12.)).border_b_1().border_color(BONE.opacity(0.07)).child(sign_in))
            .child(row("flow-claude-md", "CLAUDE.md", inv.claude_md.map_or_else(|| "none on this Mac".into(), |b| format!("your instructions, {}", kb(b))), c.claude_md, inv.claude_md.is_some(), |c, on| c.claude_md = on, cx))
            .child(row("flow-settings", "Settings", "Model, permissions, plugins. Hooks and the status line stay: they run this Mac's programs.".into(), c.settings, inv.settings, |c, on| c.settings = on, cx))
            .child(row("flow-skills", "Skills", plural(inv.skills, "skill"), c.skills, inv.skills > 0, |c, on| c.skills = on, cx))
            .child(row("flow-commands", "Commands", plural(inv.commands, "command"), c.commands, inv.commands > 0, |c, on| c.commands = on, cx))
            .child(row("flow-agents", "Agents", plural(inv.agents, "agent"), c.agents, inv.agents > 0, |c, on| c.agents = on, cx))
            .child(row("flow-mcp", "MCP servers", format!("{}. Ones that run programs on this Mac won't run there.", plural(inv.mcp, "server")), c.mcp, inv.mcp > 0, |c, on| c.mcp = on, cx))
            .child(row("flow-git", "Git identity", inv.git.as_ref().map_or_else(|| "not set on this Mac".into(), |(n, e)| format!("{n} <{e}>")), c.git, inv.git.is_some(), |c, on| c.git = on, cx))
            .child(row("flow-gh", "GitHub sign-in", if inv.gh { "Lets agents there push and open pull requests as you.".into() } else { "The GitHub CLI isn't signed in on this Mac.".into() }, c.gh, inv.gh, |c, on| c.gh = on, cx))
            .into_any_element()
    }

    /// setting up, as it goes: what it's doing, line by line; then ready.
    fn flow_progress(&self) -> AnyElement {
        let mono = theme::mono();
        let line = |glyph: &'static str, ink: Hsla, text: String| h_flex().gap(px(10.)).items_start().child(div().w(px(12.)).flex_shrink_0().text_color(ink).child(glyph)).child(div().flex_1().min_w_0().text_color(ink).child(text));
        let mut log = v_flex().gap(px(10.)).font_family(mono).text_size(px(13.)).line_height(px(18.));
        let Some(run) = self.flow_run() else { return log.child(line("·", STONE, "Starting…".into())).into_any_element() };
        let n = run.steps.len();
        for (i, (s, notes)) in run.steps.iter().enumerate() {
            let last = i + 1 == n;
            let (glyph, ink) = match (&run.done, last) {
                (None, true) => ("›", BONE),
                (Some(Err(_)), true) => ("▲", AMBER),
                _ => ("·", STONE),
            };
            log = log.child(line(glyph, ink, s.clone()));
            for note in notes {
                log = log.child(div().pl(px(22.)).text_color(ASH).child(note.clone()));
            }
        }
        match &run.done {
            Some(Ok(())) => log = log.child(line("✓", MINT, format!("Ready, in {}s", run.started.elapsed().as_secs()))),
            Some(Err(e)) => log = log.child(div().pt(px(6.)).text_color(AMBER).child(e.clone())),
            None => {}
        }
        log.into_any_element()
    }
}

/// the landing page's colours: bone on ink, stone and ash for what's quieter.
const BONE: Hsla = Hsla { h: 0.105, s: 0.33, l: 0.89, a: 1.0 };
const STONE: Hsla = Hsla { h: 0.11, s: 0.04, l: 0.53, a: 1.0 };
const ASH: Hsla = Hsla { h: 0.11, s: 0.03, l: 0.40, a: 1.0 };
const MINT: Hsla = Hsla { h: 0.32, s: 0.42, l: 0.72, a: 1.0 };
const AMBER: Hsla = Hsla { h: 0.064, s: 0.79, l: 0.68, a: 1.0 };

/// a pill, as the landing page's: bone with ink letters when it's the one to press, glass otherwise.
fn chip(id: &'static str, label: &'static str, primary: bool) -> gpui_kit::base::test_support::Observed<Stateful<Div>> {
    div()
        .id(id)
        .test_support()
        .px(px(16.))
        .py(px(8.))
        .rounded(px(12.))
        .text_size(px(14.))
        .font_weight(FontWeight::MEDIUM)
        .when(primary, |d| d.bg(BONE).text_color(gpui_kit::rgb(0x07080a)).hover(|d| d.bg(BONE.opacity(0.88))))
        .when(!primary, |d| d.bg(BONE.opacity(0.07)).border_1().border_color(BONE.opacity(0.08)).text_color(BONE).hover(|d| d.bg(BONE.opacity(0.11))))
        .child(label)
}

/// the meadow's key, for the whole field (`frost` false) or a frosted pane of it at `b`.
fn meadow_key(b: Bounds<Pixels>, field: Bounds<Pixels>, scale: f32, frame: u32, frost: bool) -> backdrop::Key {
    let d = |p: Pixels| (f32::from(p) * scale).round().max(0.0) as u32;
    backdrop::Key {
        w: d(b.size.width),
        h: d(b.size.height),
        cell: (6.0 * scale).round() as u32,
        hue: 0.0,
        dark: true,
        bg: [4, 6, 12],
        felt: false,
        meadow: true,
        frame,
        frost,
        ox: d(b.origin.x - field.origin.x),
        oy: d(b.origin.y - field.origin.y),
        fw: d(field.size.width),
        fh: d(field.size.height),
    }
}
