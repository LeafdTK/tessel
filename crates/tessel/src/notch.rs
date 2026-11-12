//! the notch: what the agents are doing, around the camera.
//!
//! a black shape that grows out of the notch. while anything works or wants
//! you it has two ears: on the left tessel's tile, turning while work goes on;
//! on the right what it amounts to (how long, how many, or what it wants of
//! you). while everything rests it is the notch and nothing more. hover it and
//! it drops open into every agent at work. click one to go to it.
//!
//! no coloured dots up here: an orange or green dot beside the notch is macos
//! saying the microphone or camera is on, and nothing should look like that.
//!
//! the outline is apple's continuous corner (curvature that ramps up instead
//! of a circle stuck onto a straight edge) at the bottom, and small concave
//! shoulders at the top, so it reads as part of the bezel and not a box on it.
//!
//! it is its own window, a panel above the menu bar on every space, drawn from
//! the main window's model, so it costs nothing of its own: it repaints when
//! the shell does, which while something works is the shell's beat. the
//! window only ever grows before the shape does and shrinks after it has, so
//! a resize never shows.

use crate::model::{ago, project_name};
use crate::motion::{Preset, Spring, Ticker};
use crate::shell::{Shell, now_ms, state_word, trace};
use crate::theme::Palette;
use gpui_kit::component::{Icon, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use std::time::Duration;
use std::rc::Rc;
use tessel_proto::{AttentionReason, RequestId, RunState, SessionId, SessionSummary};

/// how much a session wants, least first. the notch shows the most.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Want {
    Rest,
    Done,
    Working,
    Broken,
    /// blocked on you.
    Waiting,
}

pub(crate) fn want(s: &SessionSummary) -> Want {
    match (&s.run_state, s.attention.as_ref().map(|a| a.reason)) {
        (RunState::NeedsPermission | RunState::NeedsInput, _) | (_, Some(AttentionReason::NeedsPermission | AttentionReason::NeedsInput)) => Want::Waiting,
        (RunState::Errored, _) | (_, Some(AttentionReason::Errored)) => Want::Broken,
        // a stalled turn is still open: it's working as far as anyone glancing knows.
        (RunState::Working | RunState::Starting | RunState::Stalled, _) => Want::Working,
        (_, Some(AttentionReason::Finished)) => Want::Done,
        _ => Want::Rest,
    }
}

/// the closed notch: the most wanting state there is and how many are in it.
/// none while everything rests.
pub(crate) fn glance(sessions: &[SessionSummary]) -> Option<(Want, usize)> {
    let top = sessions.iter().map(want).max()?;
    (top > Want::Rest).then(|| (top, sessions.iter().filter(|s| want(s) == top).count()))
}

/// what the right ear says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Tail {
    /// how many, when it's more than one.
    Count(usize),
    /// how long the one working has been at it.
    Time(String),
    Icon(&'static str),
}

pub(crate) fn tail(sessions: &[SessionSummary], now: u64) -> Option<(Want, Tail)> {
    let (top, n) = glance(sessions)?;
    if n > 1 {
        return Some((top, Tail::Count(n)));
    }
    let lead = rows(sessions).into_iter().next()?;
    Some((
        top,
        match top {
            Want::Working => Tail::Time(ago(since(lead), now)),
            Want::Waiting => Tail::Icon("hand"),
            Want::Broken => Tail::Icon("triangle-alert"),
            Want::Done | Want::Rest => Tail::Icon("check"),
        },
    ))
}

/// the line across the open notch: what's going on, in words.
pub(crate) fn summary(sessions: &[SessionSummary]) -> String {
    let count = |w: Want| sessions.iter().filter(|s| want(s) == w).count();
    let parts: Vec<String> = [(Want::Waiting, "waiting"), (Want::Working, "working"), (Want::Broken, "stopped"), (Want::Done, "done")]
        .into_iter()
        .filter_map(|(w, word)| Some(count(w)).filter(|n| *n > 0).map(|n| format!("{n} {word}")))
        .collect();
    match (parts.is_empty(), sessions.len()) {
        (false, _) => parts.join(" · "),
        (true, 0) => "No agents".into(),
        (true, 1) => "1 agent".into(),
        (true, n) => format!("{n} agents"),
    }
}

/// the most rows the open notch shows.
pub(crate) const MAX_ROWS: usize = 6;
/// when nothing is going on, the open notch still shows the latest few.
const RECENT: usize = 3;

/// since when a session has been in the state it's in.
fn since(s: &SessionSummary) -> u64 {
    s.attention
        .as_ref()
        .map(|a| a.since_ms)
        .or(s.activity_since_ms.filter(|_| want(s) == Want::Working))
        .unwrap_or(s.updated_ms.max(s.created_ms))
}

/// the open notch's rows: what wants you first (longest waiting first), then
/// what's working, then, if that's under three, the latest at rest.
pub(crate) fn rows(sessions: &[SessionSummary]) -> Vec<&SessionSummary> {
    let mut busy: Vec<&SessionSummary> = sessions.iter().filter(|s| want(s) > Want::Rest).collect();
    busy.sort_by_key(|s| (std::cmp::Reverse(want(s)), since(s), s.id));
    let mut resting: Vec<&SessionSummary> = sessions.iter().filter(|s| want(s) == Want::Rest).collect();
    resting.sort_by_key(|s| std::cmp::Reverse(s.updated_ms.max(s.created_ms)));
    let fill = RECENT.saturating_sub(busy.len());
    busy.extend(resting.into_iter().take(fill));
    busy.truncate(MAX_ROWS);
    busy
}

/// where the notch is, in the screen's own coordinates (points, origin at the
/// bottom left of the main screen).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Place {
    center_x: f64,
    /// the top edge of the screen.
    top: f64,
    /// zero on a screen without a notch.
    notch_w: f32,
    /// the notch's height, or the menu bar's where there isn't one.
    notch_h: f32,
}

/// does any screen have a notch: whether the notch is on unless told otherwise.
pub fn has_notch() -> bool {
    mac::place().is_some_and(|p| p.notch_w > 0.)
}

/// each ear beside the notch.
const EAR: f32 = 38.;
const ROW_H: f32 = 52.;
const PAD: f32 = 8.;
const OPEN_W: f32 = 500.;
/// the bottom corners: the notch's own size shut, the island's open.
const SHUT_R: f32 = 10.;
const OPEN_R: f32 = 26.;
/// the concave flare where the shape meets the top of the screen.
const SHUT_SHOULDER: f32 = 6.;
const OPEN_SHOULDER: f32 = 12.;
/// room in the window for the open spring to overshoot into.
const OVER_X: f32 = 18.;
const OVER_Y: f32 = 30.;
/// opening: the island's springy grow. closing: settles without a bounce.
const OPENING: Preset = Preset { duration: 0.46, bounce: 0.22 };
const CLOSING: Preset = Preset { duration: 0.34, bounce: 0.0 };
/// how long the pointer may be gone before the notch closes, so a wobble
/// past the edge doesn't fold it under you.
const GRACE_MS: u64 = 160;

pub struct Notch {
    shell: WeakEntity<Shell>,
    main: AnyWindowHandle,
    open: Spring,
    ticker: Ticker,
    hovered: bool,
    /// the native window and the view gpui draws in, once known.
    native: Option<(usize, usize)>,
    /// the frame last given to the native window, so it only moves when it must.
    frame: Option<(f64, f64, f64, f64)>,
    shown: Option<bool>,
    /// whether the pointer is being watched (only while open).
    watching: bool,
    _observe: Subscription,
}

impl Notch {
    /// open the notch's window beside the main one.
    pub fn open(shell: &Entity<Shell>, main: AnyWindowHandle, cx: &mut App) {
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(point(px(0.), px(0.)), size(px(1.), px(1.))))),
            titlebar: None,
            focus: false,
            show: false,
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            window_background: WindowBackgroundAppearance::Transparent,
            // the notch never becomes the active window (it mustn't take focus
            // from what you're doing), and gpui draws inactive windows at 30
            // frames a second; its spring would step. it only draws while
            // moving, so full rate costs nothing the rest of the time.
            inactive_frame_interval: None,
            ..Default::default()
        };
        let shell = shell.clone();
        let opened = cx.open_window(options, move |window, cx| {
            let native = native_window(window);
            cx.new(|cx| {
                let observe = cx.observe(&shell, |_, _, cx| cx.notify());
                if std::env::var_os("TESSEL_NOTCH_DEMO").is_some() {
                    Self::demo(cx);
                }
                Self {
                    shell: shell.downgrade(),
                    main,
                    open: Spring::unit(0.),
                    ticker: Ticker::default(),
                    hovered: false,
                    native,
                    frame: None,
                    shown: None,
                    watching: false,
                    _observe: observe,
                }
            })
        });
        if let Err(e) = opened {
            eprintln!("tessel: no notch: {e}");
        }
    }

    /// `TESSEL_NOTCH_DEMO=1`: open and close by itself every few seconds,
    /// pointer or not, to watch the motion (and with TESSEL_FRAME_LOG, time it).
    fn demo(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let mut open = false;
            loop {
                cx.background_executor().timer(Duration::from_millis(2600)).await;
                open = !open;
                let alive = this.update(cx, |this, cx| {
                    this.open.to(if open { 1. } else { 0. }, if open { OPENING } else { CLOSING });
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn hover(&mut self, on: bool, cx: &mut Context<Self>) {
        if std::env::var_os("TESSEL_NOTCH_DEMO").is_some() {
            return;
        }
        if on == self.hovered {
            return;
        }
        self.hovered = on;
        if on {
            self.open.to(1., OPENING);
            self.watch(cx);
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(GRACE_MS)).await;
            let _ = this.update(cx, |this, cx| {
                if !this.hovered {
                    this.open.to(0., CLOSING);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// appkit loses track of the pointer when a window grows under it, so
    /// while open the notch asks where the pointer is, a few times a second,
    /// and stops asking once it's shut.
    fn watch(&mut self, cx: &mut Context<Self>) {
        if self.watching {
            return;
        }
        self.watching = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(80)).await;
                let open = this.update(cx, |this, cx| {
                    if !this.frame.is_some_and(mac::pointer_in) {
                        this.hover(false, cx);
                    }
                    let open = this.open.target() > 0. || this.open.is_moving();
                    this.watching = open;
                    open
                });
                if !matches!(open, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    fn close_now(&mut self, cx: &mut Context<Self>) {
        self.hovered = false;
        self.open.to(0., CLOSING);
        cx.notify();
    }

    fn go_to(&mut self, id: Option<SessionId>, cx: &mut Context<Self>) {
        self.close_now(cx);
        cx.activate(true);
        let _ = self.main.update(cx, |_, window, _| window.activate_window());
        if let (Some(id), Some(shell)) = (id, self.shell.upgrade()) {
            shell.update(cx, |s, cx| s.open(id, cx));
        }
    }

    fn answer(&mut self, id: SessionId, request: RequestId, allow: bool, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |s, cx| s.answer_in(id, request, allow, cx));
        }
    }

    fn stop(&mut self, id: SessionId, cx: &mut Context<Self>) {
        if let Some(shell) = self.shell.upgrade() {
            shell.update(cx, |s, cx| s.interrupt(id, cx));
        }
    }

    /// size and show the native window, after this frame: moving it calls
    /// back into the app, which is busy drawing until then.
    fn place_window(&mut self, frame: Option<(f64, f64, f64, f64)>, cx: &mut Context<Self>) {
        let Some(native) = self.native else { return };
        let show = frame.is_some();
        let first = self.shown.is_none();
        let moved = frame.is_some() && frame != self.frame;
        if !moved && self.shown == Some(show) {
            return;
        }
        if frame.is_some() {
            self.frame = frame;
        }
        self.shown = Some(show);
        if log_frames() {
            eprintln!("notch: window {frame:?} shown {show}");
        }
        cx.spawn(async move |_, _| {
            let (win, view) = native;
            if first {
                mac::configure(win, view);
            }
            if let Some(f) = frame {
                mac::set_frame(win, f);
            }
            mac::show(win, show);
        })
        .detach();
    }
}

impl Render for Notch {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(shell) = self.shell.upgrade() else {
            window.remove_window();
            return div().into_any_element();
        };
        let Some(place) = mac::place() else { return div().into_any_element() };
        let notched = place.notch_w > 0.;
        // first what decides the window's size, then the window, then the drawing.
        let (on, busy, n_rows) = {
            let shell = shell.read(cx);
            let sessions = &shell.model.sessions;
            (shell.ui.notch.unwrap_or(notched), glance(sessions).is_some(), rows(sessions).len())
        };

        let dt = self.ticker.dt(cx.background_executor().now());
        let moving = self.open.step(dt, cx.reduce_motion());
        if moving {
            window.request_animation_frame();
        } else {
            self.ticker.rest();
        }
        // a bounce may carry it past open; shut is a floor.
        let t = self.open.value().max(0.);
        if log_frames() {
            eprintln!("notch: {} t {t:.3} target {} moving {moving}", now_ms() % 100_000, self.open.target());
        }
        let opening = self.open.target() > 0. || moving;

        let band = place.notch_h;
        // the shut shape's body: the notch and its ears, the bare notch, or (no notch, nothing going on) nothing.
        let shut_w = match (busy, notched) {
            (true, _) => place.notch_w + 2. * EAR,
            (false, true) => place.notch_w,
            (false, false) => 0.,
        };
        let list_h = n_rows.max(1) as f32 * ROW_H;
        let open_w = OPEN_W.max(place.notch_w + 2. * EAR + 160.);
        let open_h = band + PAD + list_h + PAD;
        let frame = if !on || (shut_w == 0. && !opening) {
            None
        } else if opening {
            Some(frame_at(place, open_w + 2. * (OPEN_SHOULDER + OVER_X), open_h + OVER_Y))
        } else {
            Some(frame_at(place, shut_w.max(place.notch_w) + 2. * SHUT_SHOULDER, band))
        };
        self.place_window(frame, cx);
        if frame.is_none() {
            return div().into_any_element();
        }

        let shell = shell.read(cx);
        let p = shell.ui.theme.palette(true);
        let sessions = &shell.model.sessions;
        let now = now_ms();
        let lerp = |a: f32, b: f32| a + (b - a) * t;
        let w = lerp(shut_w, open_w);
        let h = lerp(band, open_h);
        let radius = lerp(SHUT_R, OPEN_R);
        let shoulder = SHUT_SHOULDER + (OPEN_SHOULDER - SHUT_SHOULDER) * t.min(1.);
        let fade = ((t - 0.4) / 0.6).clamp(0., 1.);
        // resting, shut, on a notch: the hardware already draws exactly this.
        let draw_shape = busy || t > 0.001;

        // the ears sit just outside the notch, wherever the shape is in its growing.
        let ear = |d: AnyElement| h_flex().w(px(EAR)).h_full().flex_shrink_0().justify_center().items_center().child(d);
        let lead = rows(sessions).into_iter().next().filter(|s| want(s) > Want::Rest);
        let left_ear: AnyElement = match lead {
            Some(s) => tile(tint(want(s), &p), want(s) == Want::Working, 15.).into_any_element(),
            None => div().into_any_element(),
        };
        let right_ear: AnyElement = match tail(sessions, now) {
            Some((top, Tail::Count(n))) => div().text_size(px(12.)).font_weight(FontWeight::MEDIUM).text_color(tint(top, &p)).child(n.to_string()).into_any_element(),
            Some((top, Tail::Time(s))) => div().text_size(px(11.)).text_color(tint(top, &p).opacity(0.85)).child(s).into_any_element(),
            Some((top, Tail::Icon(name))) => Icon::empty().path(format!("icons/{name}.svg")).size(px(14.)).text_color(tint(top, &p)).into_any_element(),
            None => div().into_any_element(),
        };
        let wing = (open_w - place.notch_w) / 2. - EAR;
        let me = cx.entity().downgrade();
        let to_app = me.clone();
        let header = h_flex()
            .h(px(band))
            .w(px(open_w))
            .flex_shrink_0()
            .items_center()
            // the words and the way into tessel live out in the wings, and come in as it opens.
            .child(
                h_flex().w(px(wing)).h_full().pl(px(18.)).items_center().opacity(fade).child(
                    div().text_size(px(12.)).text_color(p.secondary).truncate().child(summary(sessions)),
                ),
            )
            .child(ear(left_ear))
            .child(div().w(px(place.notch_w)).flex_shrink_0())
            .child(ear(right_ear))
            .child(
                h_flex().w(px(wing)).h_full().pr(px(12.)).justify_end().items_center().opacity(fade).child(
                    h_flex()
                        .id("notch-app")
                        .size(px(24.))
                        .rounded(px(7.))
                        .justify_center()
                        .items_center()
                        .cursor_pointer()
                        .hover(|d| d.bg(p.label.opacity(0.1)))
                        .child(Icon::empty().path("icons/arrow-up-right.svg").size(px(14.)).text_color(p.secondary))
                        .on_click(move |_, _, cx| {
                            let _ = to_app.update(cx, |this, cx| this.go_to(None, cx));
                        }),
                ),
            );

        let shown = rows(sessions);
        let list = v_flex().w(px(open_w)).px(px(PAD)).pt(px(PAD)).opacity(fade).children(shown.iter().enumerate().map(|(ix, s)| {
            let id = s.id;
            let (go, stop, answer) = (me.clone(), me.clone(), me.clone());
            let group = SharedString::from(format!("notch-row-{ix}"));
            let acts = Acts {
                stop: Rc::new(move |cx: &mut App| {
                    let _ = stop.update(cx, |this, cx| this.stop(id, cx));
                }),
                answer: Rc::new(move |request: RequestId, allow: bool, cx: &mut App| {
                    let _ = answer.update(cx, |this, cx| this.answer(id, request, allow, cx));
                }),
            };
            row(s, shell, &p, now, group.clone(), acts)
            .id(("notch-row", ix))
            .group(group)
            .hover(|d| d.bg(p.label.opacity(0.07)))
            .on_click(move |_, _, cx| {
                let _ = go.update(cx, |this, cx| this.go_to(Some(id), cx));
            })
        }));
        let list = list.when(shown.is_empty(), |d| {
            d.child(h_flex().h(px(ROW_H)).justify_center().items_center().text_size(px(12.)).text_color(p.tertiary).child("No agents yet"))
        });

        let outer_w = w + 2. * shoulder;
        let shape = div()
            .id("notch")
            .relative()
            .w(px(outer_w))
            .h(px(h))
            .flex_shrink_0()
            .on_hover(cx.listener(|this, on: &bool, _, cx| this.hover(*on, cx)))
            .when(draw_shape, |d| d.child(outline(w, h, radius, shoulder)))
            .child(
                div().absolute().top_0().left(px(shoulder)).w(px(w)).h(px(h)).overflow_hidden().child(
                    // laid out once at full width and centred, so growing the shape reveals it evenly.
                    v_flex().absolute().top_0().left(px((w - open_w) / 2.)).w(px(open_w)).when(draw_shape, |d| d.child(header)).child(list),
                ),
            );
        h_flex().size_full().justify_center().items_start().child(shape).into_any_element()
    }
}

/// what each state looks like up here. colour only for what wants you, and never a dot.
fn tint(w: Want, p: &Palette) -> Hsla {
    match w {
        Want::Waiting => crate::theme::info(p),
        Want::Broken => p.failed,
        Want::Working | Want::Done => p.label,
        Want::Rest => p.tertiary,
    }
}

/// tessel's tile: two quarter circles from opposite corners. while working it
/// turns a quarter at a time, which flips its pattern: hold, swing, hold.
fn tile(color: Hsla, turning: bool, size_pt: f32) -> impl IntoElement {
    const PERIOD: u64 = 1100;
    let quarters = if turning {
        let now = now_ms();
        let phase = (now % PERIOD) as f32 / PERIOD as f32;
        let swing = ((phase - 0.45) / 0.55).clamp(0., 1.);
        // ease in and out, so each quarter turn starts and lands softly.
        let eased = swing * swing * (3. - 2. * swing);
        ((now / PERIOD) % 4) as f32 + eased
    } else {
        0.
    };
    let angle = quarters * std::f32::consts::FRAC_PI_2;
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let side = f32::from(bounds.size.width);
            let c = bounds.center();
            let half = side / 2.;
            let (sin, cos) = angle.sin_cos();
            // (x, y) is relative to the tile's centre: turn it, then place it.
            let at = |x: f32, y: f32| point(c.x + px(x * cos - y * sin), c.y + px(x * sin + y * cos));
            for (ox, oy, from) in [(-half, -half, 0.0_f32), (half, half, std::f32::consts::PI)] {
                let mut b = PathBuilder::stroke(px(side * 0.2));
                let n = 12;
                for i in 0..=n {
                    let a = from + std::f32::consts::FRAC_PI_2 * i as f32 / n as f32;
                    let p = at(ox + half * a.cos(), oy + half * a.sin());
                    if i == 0 { b.move_to(p) } else { b.line_to(p) }
                }
                if let Ok(path) = b.build() {
                    window.paint_path(path, color);
                }
            }
        },
    )
    .size(px(size_pt))
    .flex_shrink_0()
}

/// the shape, painted: a body `w` by `h` hanging from the top edge, its
/// bottom corners apple's continuous curve, flaring into the top edge with
/// concave shoulders either side.
fn outline(w: f32, h: f32, r: f32, shoulder: f32) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let o = bounds.origin;
            let pt = |x: f32, y: f32| point(o.x + px(x), o.y + px(y));
            let (l, rt) = (shoulder, shoulder + w);
            // a continuous corner spans 1.528 radii along each edge; keep it inside the body.
            let r = r.min(w / 2. / 1.528_665).min((h - shoulder).max(0.) / 1.528_665).max(0.);
            let mut b = PathBuilder::fill();
            b.move_to(pt(0., 0.));
            // left shoulder: from the top edge, curving down into the body's side.
            b.cubic_bezier_to(pt(l, shoulder), pt(l * 0.62, 0.), pt(l, shoulder * 0.38));
            b.line_to(pt(l, h - 1.528_665 * r));
            corner(&mut b, &pt, (l, h), (0., 1.), (1., 0.), r);
            b.line_to(pt(rt - 1.528_665 * r, h));
            corner(&mut b, &pt, (rt, h), (1., 0.), (0., -1.), r);
            b.line_to(pt(rt, shoulder));
            b.cubic_bezier_to(pt(rt + shoulder, 0.), pt(rt, shoulder * 0.38), pt(rt + shoulder * 0.38, 0.));
            b.close();
            if let Ok(path) = b.build() {
                window.paint_path(path, black());
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(w + 2. * shoulder))
    .h(px(h))
}

/// one of apple's continuous corners: three cubics from 1.528 radii before the
/// corner to 1.528 radii after it, so the curvature grows from nothing
/// instead of jumping. `along` is the way in, `out` the way out.
fn corner(b: &mut PathBuilder, pt: &impl Fn(f32, f32) -> Point<Pixels>, (cx, cy): (f32, f32), along: (f32, f32), out: (f32, f32), r: f32) {
    // (how far before the corner along the way in, how far along the way out), in radii.
    let at = |k: f32, m: f32| pt(cx - along.0 * k * r + out.0 * m * r, cy - along.1 * k * r + out.1 * m * r);
    b.cubic_bezier_to(at(0.631_494, 0.074_911), at(1.088_493, 0.), at(0.868_407, 0.));
    b.cubic_bezier_to(at(0.074_911, 0.631_494), at(0.372_824, 0.169_060), at(0.169_060, 0.372_824));
    b.cubic_bezier_to(at(0., 1.528_665), at(0., 0.868_407), at(0., 1.088_493));
}

/// what a row's buttons do.
struct Acts {
    stop: Rc<dyn Fn(&mut App)>,
    answer: Rc<dyn Fn(RequestId, bool, &mut App)>,
}

/// one agent: its tile, what it is and where, what it's doing, and the numbers.
/// one that's asking leave gets Allow and Deny right there; one that's working,
/// a stop button when you hover it.
fn row(s: &SessionSummary, shell: &Shell, p: &Palette, now: u64, group: SharedString, acts: Acts) -> Div {
    let w = want(s);
    let detail = match (w, &s.activity) {
        (Want::Working, Some(a)) if !a.is_empty() => a.clone(),
        (Want::Done, _) => "Finished".into(),
        _ => state_word(&s.run_state).to_owned(),
    };
    let detail = if w > Want::Rest { format!("{detail} · {}", ago(since(s), now)) } else { detail };
    let place = match &s.worktree {
        Some(wt) => format!("{} / {}", project_name(s), wt.branch.rsplit('/').next().unwrap_or(&wt.branch)),
        None => project_name(s),
    };
    let chip = |text: String| div().h(px(16.)).px(px(5.)).rounded(px(4.)).bg(p.label.opacity(0.08)).text_size(px(10.5)).text_color(p.secondary).flex().items_center().child(text);
    let mut chips: Vec<Div> = Vec::new();
    if let Some(m) = &s.model {
        chips.push(chip(shell.model_name(m)));
    }
    if let (Some(used), Some(of)) = (s.context_used, s.context_window)
        && of > 0
    {
        chips.push(chip(format!("{}%", used * 100 / of)));
    }
    if s.background_tasks > 0 {
        chips.push(chip(format!("{} running", s.background_tasks)));
    }
    let top_right: AnyElement = if w == Want::Working && !s.trace.is_empty() {
        trace(&s.trace, p.label.opacity(0.45), 44., 12.).into_any_element()
    } else {
        div().text_size(px(11.)).text_color(p.tertiary).child(ago(s.updated_ms.max(s.created_ms), now)).into_any_element()
    };
    let busy = s.run_state.is_busy() && s.asking.is_none();
    let answer = |label: &'static str, allow: bool| {
        let (request, answer) = (s.asking.clone(), acts.answer.clone());
        h_flex()
            .id(if allow { "notch-allow" } else { "notch-deny" })
            .h(px(24.))
            .px(px(10.))
            .rounded(px(7.))
            .items_center()
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .when(allow, |d| d.bg(p.label).text_color(black()).hover(|d| d.bg(p.label.opacity(0.85))))
            .when(!allow, |d| d.bg(p.label.opacity(0.1)).text_color(p.label).hover(|d| d.bg(p.label.opacity(0.16))))
            .child(label)
            .on_click(move |_, _, cx| {
                cx.stop_propagation();
                if let Some(r) = request.clone() {
                    answer(r, allow, cx);
                }
            })
    };
    let asking = s.asking.is_some();
    let stop = acts.stop.clone();
    h_flex()
        .h(px(ROW_H))
        .px(px(10.))
        .gap(px(12.))
        .items_center()
        // concentric with the open notch: its radius less the gap around the rows.
        .rounded(px(OPEN_R - PAD))
        .cursor_pointer()
        .child(h_flex().size(px(18.)).flex_shrink_0().justify_center().items_center().child(tile(tint(w, p), w == Want::Working, 13.)))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(2.))
                .child(
                    h_flex()
                        .gap(px(6.))
                        .min_w_0()
                        .items_baseline()
                        .child(div().min_w_0().text_size(px(13.)).text_color(p.label).truncate().child(s.goal.clone()))
                        .child(div().flex_shrink_0().max_w(px(160.)).text_size(px(11.)).text_color(p.tertiary).truncate().child(place)),
                )
                .child(div().text_size(px(12.)).text_color(if w == Want::Waiting { crate::theme::info(p) } else { p.secondary }).truncate().child(detail)),
        )
        .when(!asking, |d| d.child(v_flex().flex_shrink_0().items_end().gap(px(5.)).child(top_right).child(h_flex().gap(px(4.)).children(chips))))
        .when(asking, |d| d.child(h_flex().flex_shrink_0().gap(px(6.)).child(answer("Deny", false)).child(answer("Allow", true))))
        .when(busy, |d| {
            d.child(
                h_flex()
                    .id("notch-stop")
                    .size(px(24.))
                    .flex_shrink_0()
                    .rounded(px(7.))
                    .justify_center()
                    .items_center()
                    .opacity(0.)
                    .group_hover(group, |s| s.opacity(1.))
                    .hover(|d| d.bg(p.label.opacity(0.12)))
                    .child(Icon::empty().path("icons/square.svg").size(px(12.)).text_color(p.secondary))
                    .on_click(move |_, _, cx| {
                        cx.stop_propagation();
                        stop(cx);
                    }),
            )
        })
}

fn log_frames() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("TESSEL_FRAME_LOG").is_some())
}

/// a window `w` by `h` hanging from the top of the screen, centred on the
/// notch. every size is an even number of points and none is rounded off
/// the centre, so the shape doesn't shift half a point when the window swaps.
fn frame_at(place: Place, w: f32, h: f32) -> (f64, f64, f64, f64) {
    let w = (f64::from(w) / 2.).ceil() * 2.;
    let h = f64::from(h).ceil();
    (place.center_x - w / 2., place.top - h, w, h)
}

fn native_window(window: &Window) -> Option<(usize, usize)> {
    #[cfg(target_os = "macos")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::AppKit(h) => {
                let view = h.ns_view.as_ptr() as usize;
                mac::window_of_view(view).map(|w| (w, view))
            }
            _ => None,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        None
    }
}

#[cfg(target_os = "macos")]
mod mac {
    //! the few appkit calls gpui has no words for. plain messages with our own
    //! copies of the geometry structs, so nothing new is compiled for them.
    use super::Place;
    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::{AnyObject, Bool};
    use objc2::{class, msg_send, sel};

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Pt {
        x: f64,
        y: f64,
    }
    unsafe impl Encode for Pt {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Sz {
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Sz {
        const ENCODING: Encoding = Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Rect {
        o: Pt,
        s: Sz,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Pt::ENCODING, Sz::ENCODING]);
    }
    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
    struct Insets {
        top: f64,
        left: f64,
        bottom: f64,
        right: f64,
    }
    unsafe impl Encode for Insets {
        const ENCODING: Encoding = Encoding::Struct("NSEdgeInsets", &[f64::ENCODING, f64::ENCODING, f64::ENCODING, f64::ENCODING]);
    }

    /// the screen with a notch, or failing that the main screen and its menu
    /// bar. every notched macbook says where its notch is the same way (the
    /// safe area's top, and the two usable strips either side of it), so
    /// nothing here knows any one model's size.
    pub(super) fn place() -> Option<Place> {
        unsafe {
            let screens: *mut AnyObject = msg_send![class!(NSScreen), screens];
            if screens.is_null() {
                return None;
            }
            let n: usize = msg_send![screens, count];
            for i in 0..n {
                let s: *mut AnyObject = msg_send![screens, objectAtIndex: i];
                // macos 12 and later; every macbook with a notch shipped with 12.
                let knows: Bool = msg_send![s, respondsToSelector: sel!(safeAreaInsets)];
                if !knows.as_bool() {
                    continue;
                }
                let insets: Insets = msg_send![s, safeAreaInsets];
                if insets.top <= 0. {
                    continue;
                }
                let f: Rect = msg_send![s, frame];
                let l: Rect = msg_send![s, auxiliaryTopLeftArea];
                let r: Rect = msg_send![s, auxiliaryTopRightArea];
                let w = f.s.w - l.s.w - r.s.w;
                if !(60.0..=f.s.w / 2.).contains(&w) || l.s.w <= 0. {
                    continue;
                }
                return Some(Place { center_x: f.o.x + l.s.w + w / 2., top: f.o.y + f.s.h, notch_w: w as f32, notch_h: insets.top as f32 });
            }
            let s: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
            if s.is_null() {
                return None;
            }
            let f: Rect = msg_send![s, frame];
            let v: Rect = msg_send![s, visibleFrame];
            let bar = (f.o.y + f.s.h) - (v.o.y + v.s.h);
            Some(Place { center_x: f.o.x + f.s.w / 2., top: f.o.y + f.s.h, notch_w: 0., notch_h: bar.clamp(24., 40.) as f32 })
        }
    }

    /// is the pointer over this frame (in screen coordinates).
    pub(super) fn pointer_in((x, y, w, h): (f64, f64, f64, f64)) -> bool {
        let p: Pt = unsafe { msg_send![class!(NSEvent), mouseLocation] };
        p.x >= x && p.x <= x + w && p.y >= y && p.y <= y + h
    }

    pub(super) fn window_of_view(view: usize) -> Option<usize> {
        let w: *mut AnyObject = unsafe { msg_send![view as *mut AnyObject, window] };
        (!w.is_null()).then_some(w as usize)
    }

    /// no title bar (so it may sit over the menu bar), never takes focus, and
    /// stays put when the app is in the background or hidden. and when the
    /// window changes size, whatever was last drawn stays pinned to the top
    /// instead of being stretched over the new size for a frame: everything
    /// in it hangs from the top centre, so a frame drawn for the old size is
    /// still right until the new one lands.
    pub(super) fn configure(win: usize, view: usize) {
        let win = win as *mut AnyObject;
        unsafe {
            let layer: *mut AnyObject = msg_send![view as *mut AnyObject, layer];
            if !layer.is_null() {
                let top: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"top".as_ptr()];
                let _: () = msg_send![layer, setContentsGravity: top];
            }
        }
        // borderless, non-activating panel.
        const NON_ACTIVATING: usize = 1 << 7;
        unsafe {
            let _: () = msg_send![win, setStyleMask: NON_ACTIVATING];
            let _: () = msg_send![win, setHidesOnDeactivate: Bool::NO];
            let _: () = msg_send![win, setCanHide: Bool::NO];
            let _: () = msg_send![win, setHasShadow: Bool::NO];
        }
    }

    pub(super) fn set_frame(win: usize, (x, y, w, h): (f64, f64, f64, f64)) {
        let rect = Rect { o: Pt { x, y }, s: Sz { w, h } };
        unsafe {
            let _: () = msg_send![win as *mut AnyObject, setFrame: rect, display: Bool::YES];
        }
    }

    pub(super) fn show(win: usize, show: bool) {
        let win = win as *mut AnyObject;
        unsafe {
            if show {
                let _: () = msg_send![win, orderFrontRegardless];
            } else {
                let _: () = msg_send![win, orderOut: std::ptr::null_mut::<AnyObject>()];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod mac {
    use super::Place;
    pub(super) fn place() -> Option<Place> {
        None
    }
    pub(super) fn pointer_in(_: (f64, f64, f64, f64)) -> bool {
        false
    }
    pub(super) fn configure(_: usize, _: usize) {}
    pub(super) fn set_frame(_: usize, _: (f64, f64, f64, f64)) {}
    pub(super) fn show(_: usize, _: bool) {}
}

#[cfg(test)]
mod tests {
    use super::{MAX_ROWS, Tail, Want, glance, rows, summary, tail};
    use tessel_proto::{Attention, AttentionReason, Millis, RunState, SessionSummary};

    fn s(n: u128, run: RunState, attention: Option<AttentionReason>, at: Millis) -> SessionSummary {
        let mut s: SessionSummary = serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::from_u128(n), "goal": format!("s{n}"), "cwd": "/w", "run_state": run,
            "created_ms": at, "updated_ms": at, "last_seq": 0,
        }))
        .expect("a summary");
        s.attention = attention.map(|reason| Attention { reason, since_ms: at, seq: 0 });
        s
    }

    #[test]
    fn the_shut_notch_shows_whatever_wants_most_and_how_many() {
        assert_eq!(glance(&[]), None);
        assert_eq!(glance(&[s(1, RunState::Idle, None, 1)]), None, "all at rest: just the notch");
        let busy = [s(1, RunState::Working, None, 1), s(2, RunState::Working, None, 2), s(3, RunState::Idle, Some(AttentionReason::Finished), 3)];
        assert_eq!(glance(&busy), Some((Want::Working, 2)));
        let blocked = [s(1, RunState::Working, None, 1), s(2, RunState::NeedsPermission, Some(AttentionReason::NeedsPermission), 2)];
        assert_eq!(glance(&blocked), Some((Want::Waiting, 1)), "waiting on you outranks working");
    }

    #[test]
    fn the_right_ear_says_how_many_how_long_or_what_it_wants() {
        let one = [s(1, RunState::Working, None, 1_000), s(2, RunState::Idle, None, 0)];
        assert_eq!(tail(&one, 43_000), Some((Want::Working, Tail::Time("42s".into()))));
        let two = [s(1, RunState::Working, None, 1), s(2, RunState::Working, None, 2)];
        assert_eq!(tail(&two, 9), Some((Want::Working, Tail::Count(2))));
        let asks = [s(1, RunState::NeedsInput, Some(AttentionReason::NeedsInput), 1)];
        assert_eq!(tail(&asks, 9), Some((Want::Waiting, Tail::Icon("hand"))));
        let done = [s(1, RunState::Idle, Some(AttentionReason::Finished), 1)];
        assert_eq!(tail(&done, 9), Some((Want::Done, Tail::Icon("check"))));
        assert_eq!(tail(&[s(1, RunState::Idle, None, 1)], 9), None);
    }

    #[test]
    fn the_open_notch_says_it_in_words() {
        assert_eq!(summary(&[]), "No agents");
        assert_eq!(summary(&[s(1, RunState::Idle, None, 1), s(2, RunState::Idle, None, 1)]), "2 agents");
        let mixed = [s(1, RunState::Working, None, 1), s(2, RunState::Working, None, 1), s(3, RunState::NeedsPermission, Some(AttentionReason::NeedsPermission), 1)];
        assert_eq!(summary(&mixed), "1 waiting · 2 working");
    }

    #[test]
    fn the_open_notch_lists_what_wants_you_first_then_the_latest_at_rest() {
        let all = [
            s(1, RunState::Idle, None, 10),
            s(2, RunState::Working, None, 5),
            s(3, RunState::NeedsInput, Some(AttentionReason::NeedsInput), 9),
            s(4, RunState::NeedsPermission, Some(AttentionReason::NeedsPermission), 7),
            s(5, RunState::Idle, None, 50),
        ];
        let ids: Vec<u128> = rows(&all).iter().map(|s| s.id.0.as_u128()).collect();
        assert_eq!(ids, vec![4, 3, 2], "waiting longest first, then working; three busy leaves no room for resting ones");
        let quiet = [s(1, RunState::Idle, None, 10), s(2, RunState::Idle, None, 30), s(5, RunState::Idle, None, 20), s(6, RunState::Idle, None, 5)];
        let ids: Vec<u128> = rows(&quiet).iter().map(|s| s.id.0.as_u128()).collect();
        assert_eq!(ids, vec![2, 5, 1], "nothing going on: the latest three");
        let many: Vec<SessionSummary> = (0..10).map(|n| s(n, RunState::Working, None, n as Millis)).collect();
        assert_eq!(rows(&many).len(), MAX_ROWS);
    }
}
