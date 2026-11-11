//! the terminal module: a shell the daemon runs, drawn here.
//!
//! the daemon owns the pty and its program; this window keeps its own copy of
//! the screen (ghostty's emulator again, fed the same bytes) and paints it.
//! painting is cell-exact: each run of same-styled cells is shaped once and
//! kept until that row changes, so a frame where one line changed reshapes one
//! line. keys go back as the program asked for them (ghostty's key encoder).

use super::*;
use crate::ui::Module;
use gpui_kit::{
    Bounds, ClipboardItem, Font, FontStyle, KeyDownEvent, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollDelta, ScrollEvent,
    ShapedLine, StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, canvas, fill, point,
};
use photon_proto::term::{TermId, TermInfo};
use photon_term::{Key, KeyAction, Mods, Rgb, Screen, Term};
use std::cell::RefCell;
use std::rc::Rc;

/// a terminal's history, in rows, in this window's copy.
const SCROLLBACK: usize = 10_000;
/// the room between the pane's edge and the first cell.
const PAD: f32 = 10.0;

/// where glyphs the mono face lacks are looked for: shell prompts draw
/// powerline arrows and nerd-font icons, which only these carry. whichever is
/// installed is used; one that isn't costs nothing.
const SYMBOL_FONTS: &[&str] = &[
    "Symbols Nerd Font Mono",
    "JetBrainsMonoNL Nerd Font",
    "JetBrainsMono Nerd Font",
    "Hack Nerd Font Mono",
    "MesloLGS Nerd Font",
    "FiraCode Nerd Font",
    "Apple Symbols",
];

fn term_font(family: SharedString, bold: bool, italic: bool) -> Font {
    Font {
        family,
        weight: if bold { FontWeight::BOLD } else { FontWeight::NORMAL },
        style: if italic { FontStyle::Italic } else { FontStyle::Normal },
        fallbacks: Some(gpui_kit::FontFallbacks::from_fonts(SYMBOL_FONTS.iter().map(|s| (*s).to_owned()).collect())),
        ..Font::default()
    }
}

/// one run of cells, shaped: where it starts, how wide, its background.
struct Shaped {
    col: u16,
    cols: u16,
    bg: Option<Hsla>,
    line: ShapedLine,
}

pub(crate) struct TermView {
    pub(crate) info: TermInfo,
    term: Term,
    screen: Screen,
    /// per row: the version it was shaped at, and its runs.
    shaped: Vec<(u64, Vec<Shaped>)>,
    /// cols, rows this window last told the daemon.
    size: (u16, u16),
    /// receiving its bytes (it was sent a dump to start from).
    attached: bool,
    /// bytes arrived since the screen was last brought up to date.
    fresh: bool,
    colors: Option<(Rgb, Rgb, Rgb)>,
    pub(crate) focus: FocusHandle,
    /// where it was painted and its cell size, for the mouse.
    bounds: Bounds<Pixels>,
    cell: (Pixels, Pixels),
    selecting: bool,
    scroll_acc: f32,
}

pub(crate) type TermCell = Rc<RefCell<TermView>>;

fn rgb(c: Rgb) -> Hsla {
    Rgba { r: f32::from(c.0) / 255.0, g: f32::from(c.1) / 255.0, b: f32::from(c.2) / 255.0, a: 1.0 }.into()
}

fn to_rgb(c: Hsla) -> Rgb {
    let c = c.to_rgb();
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Rgb(b(c.r), b(c.g), b(c.b))
}

/// a key's name, as gpui gives it, as ghostty knows it; and what it types unshifted.
fn key_of(name: &str) -> Option<(Key, char)> {
    let k = match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "space" => return Some((Key::Space, ' ')),
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "up" => Key::ArrowUp,
        "down" => Key::ArrowDown,
        "left" => Key::ArrowLeft,
        "right" => Key::ArrowRight,
        "f1" => Key::F1,
        "f2" => Key::F2,
        "f3" => Key::F3,
        "f4" => Key::F4,
        "f5" => Key::F5,
        "f6" => Key::F6,
        "f7" => Key::F7,
        "f8" => Key::F8,
        "f9" => Key::F9,
        "f10" => Key::F10,
        "f11" => Key::F11,
        "f12" => Key::F12,
        _ => {
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else { return None };
            let k = match c.to_ascii_lowercase() {
                'a' => Key::A,
                'b' => Key::B,
                'c' => Key::C,
                'd' => Key::D,
                'e' => Key::E,
                'f' => Key::F,
                'g' => Key::G,
                'h' => Key::H,
                'i' => Key::I,
                'j' => Key::J,
                'k' => Key::K,
                'l' => Key::L,
                'm' => Key::M,
                'n' => Key::N,
                'o' => Key::O,
                'p' => Key::P,
                'q' => Key::Q,
                'r' => Key::R,
                's' => Key::S,
                't' => Key::T,
                'u' => Key::U,
                'v' => Key::V,
                'w' => Key::W,
                'x' => Key::X,
                'y' => Key::Y,
                'z' => Key::Z,
                '0' => Key::Digit0,
                '1' => Key::Digit1,
                '2' => Key::Digit2,
                '3' => Key::Digit3,
                '4' => Key::Digit4,
                '5' => Key::Digit5,
                '6' => Key::Digit6,
                '7' => Key::Digit7,
                '8' => Key::Digit8,
                '9' => Key::Digit9,
                '-' => Key::Minus,
                '=' => Key::Equal,
                '[' => Key::BracketLeft,
                ']' => Key::BracketRight,
                '\\' => Key::Backslash,
                ';' => Key::Semicolon,
                '\'' => Key::Quote,
                ',' => Key::Comma,
                '.' => Key::Period,
                '/' => Key::Slash,
                '`' => Key::Backquote,
                _ => Key::Unidentified,
            };
            return Some((k, c));
        }
    };
    Some((k, '\0'))
}

impl TermView {
    fn new(info: TermInfo, cx: &mut App) -> Self {
        let term = Term::new(info.cols.max(2), info.rows.max(1), SCROLLBACK).expect("a terminal can always be made");
        Self {
            size: (info.cols, info.rows),
            info,
            term,
            screen: Screen::default(),
            shaped: Vec::new(),
            attached: false,
            fresh: true,
            colors: None,
            focus: cx.focus_handle(),
            bounds: Bounds::default(),
            cell: (px(8.), px(16.)),
            selecting: false,
            scroll_acc: 0.0,
        }
    }

    /// start over: the next attach brings a dump of everything.
    fn reset(&mut self) {
        self.term.reset();
        self.screen = Screen::default();
        self.shaped.clear();
        self.attached = false;
        self.fresh = true;
    }

    /// the cell under a point in the window.
    fn cell_at(&self, p: Point<Pixels>) -> (u16, u16) {
        let x = ((p.x - self.bounds.origin.x - px(PAD)) / self.cell.0).floor().max(0.0);
        let y = ((p.y - self.bounds.origin.y - px(PAD)) / self.cell.1).floor().max(0.0);
        ((x as u16).min(self.size.0.saturating_sub(1)), (y as u16).min(self.size.1.saturating_sub(1)))
    }
}

impl Shell {
    /// the terminal a pane shows for the open session, if it has one yet.
    fn pane_term(&self, pane: &str) -> Option<TermId> {
        let session = self.model.open_summary().filter(|_| !self.show_new).map(|s| s.id);
        self.pane_terms.get(&(pane.to_owned(), session)).copied().filter(|id| self.terms.contains_key(id))
    }

    /// every terminal pane on screen has a terminal, attached. a pane shows the
    /// open session's terminal: one already running for it that no other pane
    /// shows, or a new one in its folder.
    pub(crate) fn sync_terminals(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let session = self.model.open_summary().filter(|_| !self.show_new).map(|s| s.id);
        let panes: Vec<String> = self.ui.panes().into_iter().filter(|p| p.module == Some(Module::Terminal) && self.pane_open(p)).map(|p| p.id.clone()).collect();
        for pane in panes {
            let key = (pane.clone(), session);
            let bound = self.pane_terms.get(&key).copied().filter(|id| self.terms.contains_key(id));
            let id = match bound {
                Some(id) => id,
                None if self.term_opening.contains(&key) => continue,
                None => {
                    let shown: std::collections::HashSet<TermId> = self.pane_terms.values().copied().collect();
                    let free = self.terms.iter().find(|(id, v)| {
                        let v = v.borrow();
                        v.info.session == session && v.info.exited.is_none() && !shown.contains(id)
                    });
                    match free {
                        Some((id, _)) => {
                            self.pane_terms.insert(key, *id);
                            *id
                        }
                        None => {
                            self.term_opening.insert(key);
                            self.cmd(Cmd::TermOpen { pane, session, cwd: None, cols: 100, rows: 30 });
                            continue;
                        }
                    }
                }
            };
            if let Some(v) = self.terms.get(&id) {
                let mut v = v.borrow_mut();
                if !v.attached {
                    v.attached = true;
                    let _ = self.bridge.cmds.send(Cmd::TermAttach(id));
                }
            }
        }
        // a terminal just asked for (⌃`, terminal.new) takes the keys once it's up.
        if let Some(pane) = self.term_focus.clone()
            && let Some(id) = self.pane_term(&pane)
            && let Some(v) = self.terms.get(&id)
        {
            let focus = v.borrow().focus.clone();
            window.focus(&focus, cx);
            self.term_focus = None;
        }
    }

    /// a terminal in a new pane below the focused one.
    pub(crate) fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let at = self.focused_pane()?;
        let pane = self.split_pane(at.trail, crate::ui::Axis::Column, window, cx)?;
        let trail = self.ui.panes().into_iter().find(|p| p.id == pane)?.trail.clone();
        self.set_pane_module(trail, Module::Terminal, window, cx);
        self.term_focus = Some(pane.clone());
        Some(pane)
    }

    pub(crate) fn term_event(&mut self, ev: UiEvent, cx: &mut Context<Self>) {
        match ev {
            UiEvent::Terms(list) => {
                let ids: std::collections::HashSet<TermId> = list.iter().map(|t| t.id).collect();
                self.terms.retain(|id, _| ids.contains(id));
                self.pane_terms.retain(|_, id| ids.contains(id));
                for info in list {
                    match self.terms.get(&info.id) {
                        Some(v) => {
                            let mut v = v.borrow_mut();
                            v.info = info;
                            v.reset();
                        }
                        None => {
                            let view = TermView::new(info.clone(), cx);
                            self.terms.insert(info.id, Rc::new(RefCell::new(view)));
                        }
                    }
                }
            }
            UiEvent::TermOpened { pane, session, info } => {
                let key = (pane, session);
                self.term_opening.remove(&key);
                let id = info.id;
                if !self.terms.contains_key(&id) {
                    let view = TermView::new(info, cx);
                    self.terms.insert(id, Rc::new(RefCell::new(view)));
                }
                self.pane_terms.insert(key, id);
            }
            UiEvent::TermOutput(id, bytes) => {
                if let Some(v) = self.terms.get(&id) {
                    let mut v = v.borrow_mut();
                    v.term.feed(&bytes);
                    v.fresh = true;
                }
            }
            UiEvent::TermUpdated(info) => match self.terms.get(&info.id) {
                Some(v) => v.borrow_mut().info = info,
                None => {
                    let view = TermView::new(info.clone(), cx);
                    self.terms.insert(info.id, Rc::new(RefCell::new(view)));
                }
            },
            UiEvent::TermRemoved(id) => {
                self.terms.remove(&id);
                self.pane_terms.retain(|_, t| *t != id);
            }
            _ => {}
        }
        cx.notify();
    }

    /// the title a terminal pane wears: what the program says, else its folder.
    pub(crate) fn term_title(&self, pane: &str) -> Option<(String, Option<i32>)> {
        let v = self.terms.get(&self.pane_term(pane)?)?.borrow();
        let title = if v.info.title.trim().is_empty() {
            let home = std::env::var("HOME").unwrap_or_default();
            match v.info.cwd.strip_prefix(&home) {
                Some(rest) if !home.is_empty() => format!("~{rest}"),
                _ => v.info.cwd.clone(),
            }
        } else {
            v.info.title.clone()
        };
        Some((title, v.info.exited))
    }

    fn term_key(&mut self, id: TermId, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.terms.get(&id).cloned() else { return };
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        let mut v = view.borrow_mut();
        // ⌘ is the window's, except copy and paste.
        if m.platform {
            match ks.key.as_str() {
                "c" => {
                    if let Some(text) = v.term.selected_text() {
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        cx.stop_propagation();
                    }
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
                        let bytes = v.term.paste(&text);
                        v.term.scroll_to_bottom();
                        v.fresh = true;
                        let _ = self.bridge.cmds.send(Cmd::TermInput(id, bytes));
                        cx.stop_propagation();
                    }
                }
                _ => {}
            }
            return;
        }
        // a shell that has exited: return starts a new one in its place.
        if v.info.exited.is_some() {
            if ks.key == "enter" {
                drop(v);
                self.cmd(Cmd::TermClose(id));
                self.terms.remove(&id);
                self.pane_terms.retain(|_, t| *t != id);
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let Some((key, unshifted)) = key_of(&ks.key) else { return };
        let mut mods = Mods::empty();
        if m.shift {
            mods |= Mods::SHIFT;
        }
        if m.control {
            mods |= Mods::CTRL;
        }
        // option that composed a character (option-e, then e: é) typed it;
        // option on its own is alt.
        let text = ks.key_char.clone().filter(|_| !m.control);
        let composed = m.alt && text.as_deref().is_some_and(|t| t != ks.key && !t.eq_ignore_ascii_case(&ks.key));
        if m.alt && !composed {
            mods |= Mods::ALT;
        }
        let action = if ev.is_held { KeyAction::Repeat } else { KeyAction::Press };
        let bytes = v.term.key(key, mods, text.as_deref(), unshifted, action);
        if !bytes.is_empty() {
            v.term.scroll_to_bottom();
            v.term.clear_selection();
            v.fresh = true;
            let _ = self.bridge.cmds.send(Cmd::TermInput(id, bytes));
            cx.stop_propagation();
            cx.notify();
        }
        let _ = window;
    }

    /// a terminal pane's body.
    pub(crate) fn terminal_el(&self, pane: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let Some(id) = self.pane_term(pane) else {
            return div().size_full().flex().items_center().justify_center().child(gpui_kit::component::spinner::Spinner::new().small().color(p.tertiary)).into_any_element();
        };
        let view = self.terms[&id].clone();
        let focus = view.borrow().focus.clone();
        let exited = view.borrow().info.exited;
        let colors = (to_rgb(p.label), to_rgb(p.window), to_rgb(p.accent));
        let font_size = theme::z(13.0);
        let line_h = theme::z(18.0);
        let mono = theme::mono();
        let paint_view = view.clone();
        let bridge = self.bridge.cmds.clone();
        let focus_for_paint = focus.clone();
        let surface = canvas(
            move |bounds, window, _cx| {
                let mut v = paint_view.borrow_mut();
                if v.colors != Some(colors) {
                    v.colors = Some(colors);
                    v.term.set_colors(colors.0, colors.1, colors.2);
                    v.fresh = true;
                    v.shaped.clear();
                }
                let font = Font { family: mono.clone(), ..Font::default() };
                let ts = window.text_system();
                let font_id = ts.resolve_font(&font);
                let cw = ts.advance(font_id, font_size, 'M').map(|s| s.width).unwrap_or(font_size * 0.6);
                v.cell = (cw, line_h);
                v.bounds = bounds;
                let cols = (((bounds.size.width - px(2.0 * PAD)) / cw).floor() as i32).clamp(2, 1000) as u16;
                let rows = (((bounds.size.height - px(2.0 * PAD)) / line_h).floor() as i32).clamp(1, 500) as u16;
                if (cols, rows) != v.size {
                    v.size = (cols, rows);
                    let _ = v.term.resize(cols, rows, f32::from(cw) as u32, f32::from(line_h) as u32);
                    let _ = bridge.send(Cmd::TermResize(id, cols, rows));
                    v.fresh = true;
                }
                if v.fresh {
                    v.fresh = false;
                    let TermView { term, screen, .. } = &mut *v;
                    let _ = term.update(screen);
                }
                // shape the rows that changed since they were last shaped.
                let n = v.screen.lines.len();
                v.shaped.resize_with(n, || (u64::MAX, Vec::new()));
                for i in 0..n {
                    let version = v.screen.lines[i].version;
                    if v.shaped[i].0 == version {
                        continue;
                    }
                    let line = &v.screen.lines[i];
                    let runs: Vec<Shaped> = line
                        .spans
                        .iter()
                        .map(|s| {
                            let text: SharedString = line.text[s.start..s.end].to_owned().into();
                            let color = if s.faint { rgb(s.fg).opacity(0.6) } else { rgb(s.fg) };
                            let run = TextRun {
                                len: text.len(),
                                font: term_font(mono.clone(), s.bold, s.italic),
                                color,
                                background_color: None,
                                underline: s.underline.then(|| UnderlineStyle { thickness: px(1.), color: Some(color), wavy: false }),
                                strikethrough: s.strike.then(|| StrikethroughStyle { thickness: px(1.), color: Some(color) }),
                            };
                            Shaped { col: s.col, cols: s.cols, bg: s.bg.map(rgb), line: ts.shape_line(text, font_size, &[run], None) }
                        })
                        .collect();
                    v.shaped[i] = (version, runs);
                }
            },
            move |bounds, (), window, cx| {
                let v = view.borrow();
                let (cw, lh) = v.cell;
                let origin = point(bounds.origin.x + px(PAD), bounds.origin.y + px(PAD));
                window.paint_quad(fill(bounds, rgb(v.screen.bg)));
                for (row, (_, runs)) in v.shaped.iter().enumerate() {
                    let y = origin.y + lh * row as f32;
                    for r in runs {
                        if let Some(bg) = r.bg {
                            window.paint_quad(fill(Bounds::new(point(origin.x + cw * f32::from(r.col), y), gpui_kit::size(cw * f32::from(r.cols), lh)), bg));
                        }
                    }
                    for r in runs {
                        let _ = r.line.paint(point(origin.x + cw * f32::from(r.col), y), lh, TextAlign::Left, None, window, cx);
                    }
                }
                if let Some(c) = v.screen.cursor.filter(|_| v.screen.scrolled == 0) {
                    let at = point(origin.x + cw * f32::from(c.col), origin.y + lh * f32::from(c.row));
                    let ink = v.colors.map_or(rgb(v.screen.fg), |c| rgb(c.2));
                    let focused = focus_for_paint.is_focused(window);
                    let (shape, w) = match (focused, c.shape) {
                        (false, _) | (true, photon_term::CursorShape::Hollow) => (None, cw),
                        (true, photon_term::CursorShape::Bar) => (Some(Bounds::new(at, gpui_kit::size(px(2.), lh))), px(2.)),
                        (true, photon_term::CursorShape::Underline) => (Some(Bounds::new(point(at.x, at.y + lh - px(2.)), gpui_kit::size(cw, px(2.)))), cw),
                        (true, photon_term::CursorShape::Block) => (Some(Bounds::new(at, gpui_kit::size(cw, lh))), cw),
                    };
                    match shape {
                        Some(b) if w == cw && b.size.height == lh => window.paint_quad(fill(b, ink.opacity(0.55))),
                        Some(b) => window.paint_quad(fill(b, ink)),
                        None => window.paint_quad(gpui_kit::outline(Bounds::new(at, gpui_kit::size(cw, lh)), ink, gpui_kit::BorderStyle::Solid)),
                    }
                }
            },
        )
        .size_full();

        let (down, moved, up, wheel) = (id, id, id, id);
        div()
            .id(SharedString::from(format!("terminal-{id}")))
            .test_support()
            .aria_label("Terminal")
            .size_full()
            .relative()
            .track_focus(&focus)
            .key_context("Terminal")
            .cursor(gpui_kit::CursorStyle::IBeam)
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| this.term_key(id, ev, window, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, window, cx| {
                    let Some(v) = this.terms.get(&down).cloned() else { return };
                    let mut v = v.borrow_mut();
                    window.focus(&v.focus, cx);
                    if !v.term.mouse_tracking() {
                        let (c, r) = v.cell_at(ev.position);
                        v.term.select_start(c, r);
                        v.selecting = true;
                        v.fresh = true;
                    }
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
                let Some(v) = this.terms.get(&moved).cloned() else { return };
                let mut v = v.borrow_mut();
                if v.selecting && ev.pressed_button == Some(MouseButton::Left) {
                    let (c, r) = v.cell_at(ev.position);
                    v.term.select_to(c, r);
                    v.fresh = true;
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _: &MouseUpEvent, _, _| {
                    if let Some(v) = this.terms.get(&up) {
                        let mut v = v.borrow_mut();
                        v.selecting = false;
                        v.term.select_end();
                    }
                }),
            )
            .on_scroll(cx.listener(move |this, ev: &ScrollEvent, _, cx| {
                let Some(view) = this.terms.get(&wheel).cloned() else { return };
                let mut v = view.borrow_mut();
                let lines = match ev.delta {
                    ScrollDelta::Lines(l) => l.y,
                    ScrollDelta::Pixels(p) => p.y / v.cell.1,
                };
                v.scroll_acc += lines;
                let whole = v.scroll_acc.trunc();
                if whole == 0.0 {
                    return;
                }
                v.scroll_acc -= whole;
                if v.term.alternate() {
                    // a full-screen program has no history: the wheel is its arrows.
                    let key = if whole > 0.0 { Key::ArrowUp } else { Key::ArrowDown };
                    let mut bytes = Vec::new();
                    for _ in 0..(whole.abs() as usize).min(20) {
                        bytes.extend(v.term.key(key, Mods::empty(), None, '\0', KeyAction::Press));
                    }
                    let _ = this.bridge.cmds.send(Cmd::TermInput(wheel, bytes));
                } else {
                    v.term.scroll(-(whole as isize));
                    v.fresh = true;
                    cx.notify();
                }
            }))
            .child(surface)
            .when_some(exited, |d, code| {
                d.child(
                    div().absolute().bottom(px(10.)).left_0().right_0().flex().justify_center().child(
                        h_flex()
                            .gap_2()
                            .px_3()
                            .h(theme::z(28.))
                            .rounded(theme::z(theme::R_CONTROL))
                            .bg(p.raised)
                            .border_1()
                            .border_color(p.separator)
                            .shadow_sm()
                            .set(Type::Callout)
                            .text_color(p.secondary)
                            .child(if code == 0 { "Exited".to_owned() } else { format!("Exited with {code}") })
                            .child(theme::keycaps(&p, "↩"))
                            .child("for a new shell"),
                    ),
                )
            })
            .into_any_element()
    }
}
