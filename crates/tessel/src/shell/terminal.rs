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
use tessel_proto::term::{TermId, TermInfo};
use tessel_term::{Key, KeyAction, Mods, MouseAct, MouseButton as TermButton, Rgb, Screen, Term};

fn mods_of(m: gpui_kit::Modifiers) -> Mods {
    let mut out = Mods::empty();
    if m.shift {
        out |= Mods::SHIFT;
    }
    if m.control {
        out |= Mods::CTRL;
    }
    if m.alt {
        out |= Mods::ALT;
    }
    if m.platform {
        out |= Mods::SUPER;
    }
    out
}
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
    pub(crate) ime: Option<Entity<TermIme>>,
}

pub(crate) type TermCell = Rc<RefCell<TermView>>;

/// typed text for a terminal, from the platform's text input: dead keys
/// (option-e, e: é) and input methods (pinyin, kana) compose here, and what
/// they settle on is sent to the program.
pub(crate) struct TermIme {
    id: TermId,
    view: std::rc::Weak<RefCell<TermView>>,
    cmds: tokio::sync::mpsc::UnboundedSender<Cmd>,
    /// what is being composed, drawn at the cursor, not sent yet.
    pub(crate) marked: String,
}

impl gpui_kit::EntityInputHandler for TermIme {
    fn text_for_range(&mut self, _: std::ops::Range<usize>, _: &mut Option<std::ops::Range<usize>>, _: &mut Window, _: &mut Context<Self>) -> Option<String> {
        Some(self.marked.clone())
    }

    fn selected_text_range(&mut self, _: bool, _: &mut Window, _: &mut Context<Self>) -> Option<gpui_kit::UTF16Selection> {
        let n = self.marked.encode_utf16().count();
        Some(gpui_kit::UTF16Selection { range: n..n, reversed: false })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<std::ops::Range<usize>> {
        (!self.marked.is_empty()).then(|| 0..self.marked.encode_utf16().count())
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.clear();
        cx.notify();
    }

    fn replace_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, _: &mut Window, cx: &mut Context<Self>) {
        self.marked.clear();
        if text.is_empty() {
            return;
        }
        if let Some(view) = self.view.upgrade() {
            let mut v = view.borrow_mut();
            if v.info.exited.is_some() {
                return;
            }
            v.term.scroll_to_bottom();
            v.term.clear_selection();
            v.fresh = true;
        }
        let _ = self.cmds.send(Cmd::TermInput(self.id, text.as_bytes().to_vec()));
        cx.notify();
    }

    fn replace_and_mark_text_in_range(&mut self, _: Option<std::ops::Range<usize>>, text: &str, _: Option<std::ops::Range<usize>>, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = text.to_owned();
        cx.notify();
    }

    fn bounds_for_range(&mut self, _: std::ops::Range<usize>, element: Bounds<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<Bounds<Pixels>> {
        // where the cursor is: the input method puts its candidates beside it.
        let view = self.view.upgrade()?;
        let v = view.borrow();
        let c = v.screen.cursor?;
        let (cw, lh) = v.cell;
        Some(Bounds::new(point(element.origin.x + px(PAD) + cw * f32::from(c.col), element.origin.y + px(PAD) + lh * f32::from(c.row)), gpui_kit::size(cw, lh)))
    }

    fn character_index_for_point(&mut self, _: Point<Pixels>, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
        None
    }
}

fn rgb(c: Rgb) -> Hsla {
    Rgba { r: f32::from(c.0) / 255.0, g: f32::from(c.1) / 255.0, b: f32::from(c.2) / 255.0, a: 1.0 }.into()
}

/// the 16 colours programs name, for a terminal drawn on `bg`: the usual
/// hues, each made readable on that background (yellow on white, blue on
/// black). black and white stay what they are: programs use them as grounds.
fn ansi_for(bg: Hsla, accent: Hsla) -> [Rgb; 16] {
    let hue = |h: f32, s: f32, l: f32| gpui_kit::hsla(h / 360.0, s, l, 1.0);
    let dark = theme::luminance(bg, bg) < 0.2;
    let (l, lb) = if dark { (0.62, 0.72) } else { (0.42, 0.36) };
    let named = [
        (0.0, 0.70),   // red
        (135.0, 0.50), // green
        (42.0, 0.85),  // yellow
        (215.0, 0.75), // blue
        (300.0, 0.50), // magenta
        (185.0, 0.65), // cyan
    ];
    let mut out = [Rgb::default(); 16];
    let black = if dark { hue(0.0, 0.0, 0.16) } else { hue(0.0, 0.0, 0.12) };
    let white = if dark { hue(0.0, 0.0, 0.86) } else { hue(0.0, 0.0, 0.72) };
    out[0] = to_rgb(black);
    out[7] = to_rgb(white);
    out[8] = to_rgb(theme::readable(hue(0.0, 0.0, if dark { 0.5 } else { 0.48 }), bg, 3.0));
    out[15] = to_rgb(if dark { hue(0.0, 0.0, 0.98) } else { hue(0.0, 0.0, 0.84) });
    for (i, (h, s)) in named.iter().enumerate() {
        // blue leans toward the theme's accent, so a terminal belongs to its window.
        let base = if i == 3 { accent } else { hue(*h, *s, l) };
        out[1 + i] = to_rgb(theme::readable(base, bg, 3.8));
        let bright = if i == 3 { gpui_kit::Hsla { l: lb, ..accent } } else { hue(*h, *s, lb) };
        out[9 + i] = to_rgb(theme::readable(bright, bg, 3.2));
    }
    out
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
            ime: None,
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

    /// a point in the window as pixels from the grid's top left, with the cell size.
    fn grid_point(&self, p: Point<Pixels>) -> (f32, f32, (u32, u32)) {
        let x = f32::from(p.x - self.bounds.origin.x) - PAD;
        let y = f32::from(p.y - self.bounds.origin.y) - PAD;
        (x.max(0.0), y.max(0.0), (f32::from(self.cell.0).round() as u32, f32::from(self.cell.1).round() as u32))
    }

    /// the cell under a point in the window.
    fn cell_at(&self, p: Point<Pixels>) -> (u16, u16) {
        let x = ((p.x - self.bounds.origin.x - px(PAD)) / self.cell.0).floor().max(0.0);
        let y = ((p.y - self.bounds.origin.y - px(PAD)) / self.cell.1).floor().max(0.0);
        ((x as u16).min(self.size.0.saturating_sub(1)), (y as u16).min(self.size.1.saturating_sub(1)))
    }
}

impl Shell {
    #[cfg(test)]
    pub(crate) fn term_ime_for_test(&self, id: TermId) -> Entity<TermIme> {
        self.terms[&id].borrow().ime.clone().expect("every view has its input")
    }

    /// a terminal's view, with its text input, kept by id.
    fn add_term(&mut self, info: TermInfo, cx: &mut Context<Self>) {
        let id = info.id;
        let view = Rc::new(RefCell::new(TermView::new(info, cx)));
        let ime = cx.new(|_| TermIme { id, view: Rc::downgrade(&view), cmds: self.bridge.cmds.clone(), marked: String::new() });
        view.borrow_mut().ime = Some(ime);
        self.terms.insert(id, view);
    }

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
                // the daemon's terminals, and the bench's own (it has none of the daemon's).
                let mut ids: std::collections::HashSet<TermId> = list.iter().map(|t| t.id).collect();
                ids.insert(BENCH_ID);
                self.terms.retain(|id, _| ids.contains(id));
                self.pane_terms.retain(|_, id| ids.contains(id));
                for info in list {
                    match self.terms.get(&info.id) {
                        Some(v) => {
                            let mut v = v.borrow_mut();
                            v.info = info;
                            v.reset();
                        }
                        None => self.add_term(info, cx),
                    }
                }
            }
            UiEvent::TermOpened { pane, session, info } => {
                let key = (pane, session);
                self.term_opening.remove(&key);
                let id = info.id;
                if !self.terms.contains_key(&id) {
                    self.add_term(info, cx);
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
                None => self.add_term(info, cx),
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
        // plain typing comes back as text from the platform's input, which
        // knows dead keys and input methods; this handles everything else.
        let printable = text.as_deref().is_some_and(|t| !t.is_empty() && t.chars().all(|c| !c.is_control()));
        if printable && !m.control && !(m.alt && !composed) && !ev.is_held {
            return;
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

    fn term_press(&mut self, id: TermId, button: MouseButton, ev: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.terms.get(&id).cloned() else { return };
        {
            let v = view.borrow();
            window.focus(&v.focus, cx);
        }
        let mut v = view.borrow_mut();
        // a program that tracks the mouse gets the click; shift still selects.
        if v.term.mouse_tracking() && !ev.modifiers.shift {
            drop(v);
            self.term_mouse(id, MouseAct::Press, Some(button), ev.position, ev.modifiers, cx);
            return;
        }
        if button == MouseButton::Left {
            let (c, r) = v.cell_at(ev.position);
            v.term.select_start(c, r);
            v.selecting = true;
            v.fresh = true;
        }
        cx.notify();
    }

    /// a mouse move or release: to the program if it wants it, else the selection.
    fn term_mouse(&mut self, id: TermId, act: MouseAct, button: Option<MouseButton>, at: Point<Pixels>, mods: gpui_kit::Modifiers, cx: &mut Context<Self>) {
        let Some(view) = self.terms.get(&id).cloned() else { return };
        let mut v = view.borrow_mut();
        if v.selecting {
            match act {
                MouseAct::Motion if button == Some(MouseButton::Left) => {
                    let (c, r) = v.cell_at(at);
                    v.term.select_to(c, r);
                    v.fresh = true;
                    cx.notify();
                }
                MouseAct::Release => {
                    v.selecting = false;
                    v.term.select_end();
                }
                _ => {}
            }
            return;
        }
        if !v.term.mouse_tracking() {
            return;
        }
        let (x, y, cell) = v.grid_point(at);
        let b = button.map(|b| match b {
            MouseButton::Middle => TermButton::Middle,
            MouseButton::Right => TermButton::Right,
            _ => TermButton::Left,
        });
        let bytes = v.term.mouse(act, b, x, y, mods_of(mods), button.is_some() && act != MouseAct::Release, cell);
        if !bytes.is_empty() {
            let _ = self.bridge.cmds.send(Cmd::TermInput(id, bytes));
        }
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
        let ansi = ansi_for(p.window, p.accent);
        let font_size = theme::z(13.0);
        let line_h = theme::z(18.0);
        let mono = theme::mono();
        let mono_paint = mono.clone();
        let paint_view = view.clone();
        let bridge = self.bridge.cmds.clone();
        let focus_for_paint = focus.clone();
        let ime = view.borrow().ime.clone();
        let surface = canvas(
            move |bounds, window, _cx| {
                let mut v = paint_view.borrow_mut();
                if v.colors != Some(colors) {
                    v.colors = Some(colors);
                    v.term.set_colors(colors.0, colors.1, colors.2);
                    v.term.set_ansi(ansi);
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
                if let Some(ime) = &ime {
                    window.handle_input(&focus_for_paint, gpui_kit::ElementInputHandler::new(bounds, ime.clone()), cx);
                }
                let marked = ime.as_ref().map(|i| i.read(cx).marked.clone()).unwrap_or_default();
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
                // what an input method is composing, underlined at the cursor.
                if let (false, Some(c)) = (marked.is_empty(), v.screen.cursor) {
                    let at = point(origin.x + cw * f32::from(c.col), origin.y + lh * f32::from(c.row));
                    let ink = rgb(v.screen.fg);
                    let run = TextRun {
                        len: marked.len(),
                        font: term_font(mono_paint.clone(), false, false),
                        color: ink,
                        background_color: Some(rgb(v.screen.bg)),
                        underline: Some(UnderlineStyle { thickness: px(1.), color: Some(ink), wavy: false }),
                        strikethrough: None,
                    };
                    let line = window.text_system().shape_line(marked.clone().into(), font_size, &[run], None);
                    let _ = line.paint(at, lh, TextAlign::Left, None, window, cx);
                    return;
                }
                if let Some(c) = v.screen.cursor.filter(|_| v.screen.scrolled == 0) {
                    let at = point(origin.x + cw * f32::from(c.col), origin.y + lh * f32::from(c.row));
                    let ink = v.colors.map_or(rgb(v.screen.fg), |c| rgb(c.2));
                    let focused = focus_for_paint.is_focused(window);
                    let (shape, w) = match (focused, c.shape) {
                        (false, _) | (true, tessel_term::CursorShape::Hollow) => (None, cw),
                        (true, tessel_term::CursorShape::Bar) => (Some(Bounds::new(at, gpui_kit::size(px(2.), lh))), px(2.)),
                        (true, tessel_term::CursorShape::Underline) => (Some(Bounds::new(point(at.x, at.y + lh - px(2.)), gpui_kit::size(cw, px(2.)))), cw),
                        (true, tessel_term::CursorShape::Block) => (Some(Bounds::new(at, gpui_kit::size(cw, lh))), cw),
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

        let wheel = id;
        let pane = div()
            .id(SharedString::from(format!("terminal-{id}")))
            .test_support()
            .aria_label("Terminal")
            .size_full()
            .relative()
            .track_focus(&focus)
            .key_context("Terminal")
            .cursor(gpui_kit::CursorStyle::IBeam)
            .on_key_down(cx.listener(move |this, ev: &KeyDownEvent, window, cx| this.term_key(id, ev, window, cx)))
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| this.term_mouse(id, MouseAct::Motion, ev.pressed_button, ev.position, ev.modifiers, cx)))
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
                let n = (whole.abs() as usize).min(20);
                let mut bytes = Vec::new();
                if v.term.mouse_tracking() {
                    // a program that wants the mouse gets the wheel as its buttons.
                    let (x, y, cell) = v.grid_point(ev.position);
                    let b = if whole > 0.0 { TermButton::WheelUp } else { TermButton::WheelDown };
                    for _ in 0..n {
                        bytes.extend(v.term.mouse(MouseAct::Press, Some(b), x, y, mods_of(ev.modifiers), false, cell));
                    }
                } else if v.term.alternate() {
                    // a full-screen program has no history: the wheel is its arrows.
                    let key = if whole > 0.0 { Key::ArrowUp } else { Key::ArrowDown };
                    for _ in 0..n {
                        bytes.extend(v.term.key(key, Mods::empty(), None, '\0', KeyAction::Press));
                    }
                } else {
                    v.term.scroll(-(whole as isize));
                    v.fresh = true;
                    cx.notify();
                }
                if !bytes.is_empty() {
                    let _ = this.bridge.cmds.send(Cmd::TermInput(wheel, bytes));
                }
            }));
        let mut pane = pane;
        for button in [MouseButton::Left, MouseButton::Middle, MouseButton::Right] {
            pane = pane
                .on_mouse_down(button, cx.listener(move |this, ev: &MouseDownEvent, window, cx| this.term_press(id, button, ev, window, cx)))
                .on_mouse_up(button, cx.listener(move |this, ev: &MouseUpEvent, _, cx| this.term_mouse(id, MouseAct::Release, Some(button), ev.position, ev.modifiers, cx)));
        }
        pane
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

#[cfg(test)]
mod tests {
    #[test]
    fn every_named_colour_reads_on_light_and_dark() {
        for bg in [gpui_kit::hsla(0.0, 0.0, 1.0, 1.0), gpui_kit::hsla(0.0, 0.0, 0.08, 1.0), gpui_kit::hsla(0.12, 0.3, 0.96, 1.0)] {
            let accent = gpui_kit::hsla(0.6, 0.8, 0.5, 1.0);
            let ansi = super::ansi_for(bg, accent);
            for i in (1..7).chain(9..15) {
                let c = super::rgb(ansi[i]);
                assert!(crate::theme::contrast(c, bg) >= 3.0, "colour {i} on {bg:?}: {:.2}", crate::theme::contrast(c, bg));
            }
        }
    }
}

// ---- --bench-term ----------------------------------------------------------------------

/// a terminal fed from here (no daemon), one chunk a frame, timed.
#[derive(Default)]
pub(crate) struct BenchTerm {
    started: Option<std::time::Instant>,
    last: Option<std::time::Instant>,
    frames: Vec<f32>,
    fed: usize,
}

const BENCH_ID: TermId = u64::MAX;
const BENCH_TOTAL: usize = 100 * 1024 * 1024;
const BENCH_CHUNK: usize = 2 * 1024 * 1024;

impl Shell {
    pub(crate) fn step_bench_term(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(b) = self.bench_term.as_mut() else { return };
        let now = std::time::Instant::now();
        if b.started.is_none() {
            b.started = Some(now);
            let layout = r#"{"root": {"type": "panes", "w": "full", "h": "full", "p": 8, "layout": {"split": "row", "children": [{"module": "conversation", "size": 1}, {"id": "bench", "module": "terminal", "size": 2}]}}}"#;
            let _ = self.use_ui_text(layout, window, cx);
            let info = TermInfo { id: BENCH_ID, title: "bench".into(), cwd: "/".into(), cols: 120, rows: 40, session: None, command: vec![], exited: None };
            self.add_term(info, cx);
            if let Some(v) = self.terms.get(&BENCH_ID) {
                v.borrow_mut().attached = true;
            }
            let session = self.model.open_summary().filter(|_| !self.show_new).map(|s| s.id);
            self.pane_terms.insert(("bench".into(), session), BENCH_ID);
            window.request_animation_frame();
            return;
        }
        let b = self.bench_term.as_mut().expect("checked");
        if let Some(last) = b.last {
            b.frames.push(now.duration_since(last).as_secs_f32() * 1000.0);
        }
        b.last = Some(now);
        if b.fed >= BENCH_TOTAL {
            let secs = b.started.map_or(0.0, |s| s.elapsed().as_secs_f32());
            let mut f = b.frames.clone();
            f.sort_by(f32::total_cmp);
            let at = |q: f32| f.get(((f.len() as f32 - 1.0) * q) as usize).copied().unwrap_or(0.0);
            let late = f.iter().filter(|ms| **ms > 16.7).count();
            println!(
                "terminal: {} MB in {:.2} s ({:.0} MB/s), {} frames, median {:.1} ms, p99 {:.1} ms, worst {:.1} ms, {} over 16.7 ms",
                BENCH_TOTAL / (1024 * 1024),
                secs,
                (BENCH_TOTAL as f32 / (1024.0 * 1024.0)) / secs.max(0.001),
                f.len(),
                at(0.5),
                at(0.99),
                f.last().copied().unwrap_or(0.0),
                late
            );
            cx.defer(|cx| cx.quit());
            return;
        }
        // a chunk of coloured lines, as a busy build log would pour out.
        let line = "\x1b[32m  Compiling\x1b[0m tessel-bench v0.1.0 (/tmp/bench) \x1b[2mtarget/debug/deps\x1b[0m 0123456789 abcdefghij\r\n";
        let chunk: String = line.repeat(BENCH_CHUNK / line.len());
        let Some(v) = self.terms.get(&BENCH_ID) else {
            eprintln!("bench: the terminal went away");
            cx.defer(|cx| cx.quit());
            return;
        };
        {
            let mut v = v.borrow_mut();
            v.term.feed(chunk.as_bytes());
            v.fresh = true;
        }
        let b = self.bench_term.as_mut().expect("checked");
        b.fed += chunk.len();
        // the next frame comes as soon as the display can take it, nothing else waking it.
        window.request_animation_frame();
    }
}
