//! terminals, on ghostty's emulator core (libghostty-vt).
//!
//! one `Term` is one terminal's state: bytes from a program go in, and out
//! come what's on screen (`Screen`, for drawing), its text (for scripts), and
//! the keys and pastes a person types, encoded the way the program asked for.
//!
//! the daemon keeps one per terminal so it survives windows and can answer the
//! program's questions (device attributes, sizes). each window keeps its own to
//! draw from, fed the same bytes. a `Term` is not `Send`: it lives on the thread
//! that made it.

use libghostty_vt::fmt::{Format, Formatter, FormatterOptions};
use libghostty_vt::key;
use libghostty_vt::render::{CellIterator, Dirty, RenderState, RowIterator};
use libghostty_vt::screen::CellWide;
use libghostty_vt::selection::Selection;
use libghostty_vt::style::RgbColor;
use libghostty_vt::terminal::{
    ConformanceLevel, DeviceAttributeFeature, DeviceAttributes, DeviceType, Mode, Point, PointCoordinate, PrimaryDeviceAttributes, ScrollViewport,
    SecondaryDeviceAttributes,
};
use libghostty_vt::{Terminal, TerminalOptions};

pub use libghostty_vt::key::{Action as KeyAction, Key, Mods};

/// a mouse button, as a program hears it. the wheel is buttons too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseAct {
    Press,
    Release,
    Motion,
}

/// what a terminal is called to the programs in it.
pub const TERM: &str = "xterm-256color";

pub type Error = libghostty_vt::Error;
pub type Result<T> = std::result::Result<T, Error>;

/// a colour, as the terminal resolved it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl From<RgbColor> for Rgb {
    fn from(c: RgbColor) -> Self {
        Rgb(c.r, c.g, c.b)
    }
}

/// a run of cells drawn the same way. a wide character is always a run of its
/// own, so a font whose wide glyphs aren't exactly two cells can't shift the
/// rest of the line.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    /// the first cell.
    pub col: u16,
    /// how many cells it covers.
    pub cols: u16,
    /// its text, as a byte range in the line's text.
    pub start: usize,
    pub end: usize,
    pub fg: Rgb,
    /// none: the terminal's own background.
    pub bg: Option<Rgb>,
    pub bold: bool,
    pub italic: bool,
    pub faint: bool,
    pub underline: bool,
    pub strike: bool,
}

/// one row on screen.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    pub text: String,
    pub spans: Vec<Span>,
    /// bumped whenever the row changes, so a drawer can keep what it shaped.
    pub version: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    Hollow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cursor {
    pub col: u16,
    pub row: u16,
    pub shape: CursorShape,
    pub blink: bool,
}

/// what's on screen: the visible rows, the colours, the cursor.
#[derive(Clone, Debug, Default)]
pub struct Screen {
    pub cols: u16,
    pub lines: Vec<Line>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Option<Cursor>,
    /// how many rows of history are above the viewport, and how far up it is.
    pub history: usize,
    pub scrolled: usize,
    next_version: u64,
}

pub struct Term {
    t: Terminal<'static, 'static>,
    render: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    keys: key::Encoder<'static>,
    key_event: key::Event<'static>,
    mouse: libghostty_vt::mouse::Encoder<'static>,
    mouse_event: libghostty_vt::mouse::Event<'static>,
    /// where a drag-selection started, in screen (scrollback-inclusive) rows.
    anchor: Option<(u16, u32)>,
}

impl std::fmt::Debug for Term {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Term").field("cols", &self.cols()).field("rows", &self.rows()).finish()
    }
}

impl Term {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Result<Self> {
        let t = Terminal::new(TerminalOptions { cols: cols.max(1), rows: rows.max(1), max_scrollback: scrollback })?;
        Ok(Self {
            t,
            render: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            keys: key::Encoder::new()?,
            key_event: key::Event::new()?,
            mouse: libghostty_vt::mouse::Encoder::new()?,
            mouse_event: libghostty_vt::mouse::Event::new()?,
            anchor: None,
        })
    }

    /// answer the program's questions (device attributes, its size, ...) by
    /// writing to it. only one side of a terminal may: the daemon's.
    pub fn answer_with(&mut self, mut write: impl FnMut(&[u8]) + 'static) -> Result<()> {
        self.t
            .on_pty_write(move |_t, data| write(data))?
            .on_device_attributes(|_t| {
                Some(DeviceAttributes {
                    primary: PrimaryDeviceAttributes::new(
                        ConformanceLevel::VT220,
                        &[DeviceAttributeFeature::COLUMNS_132, DeviceAttributeFeature::SELECTIVE_ERASE, DeviceAttributeFeature::ANSI_COLOR],
                    ),
                    secondary: SecondaryDeviceAttributes { device_type: DeviceType::VT220, firmware_version: 1, rom_cartridge: 0 },
                    tertiary: Default::default(),
                })
            })?
            .on_xtversion(|_t| Some("photon"))?;
        Ok(())
    }

    /// the colours text and background have when the program doesn't choose,
    /// and the cursor's: the window's own, so a terminal looks like the rest.
    pub fn set_colors(&mut self, fg: Rgb, bg: Rgb, cursor: Rgb) {
        let c = |c: Rgb| Some(RgbColor { r: c.0, g: c.1, b: c.2 });
        let _ = self.t.set_default_fg_color(c(fg));
        let _ = self.t.set_default_bg_color(c(bg));
        let _ = self.t.set_default_cursor_color(c(cursor));
    }

    /// the first 16 colours (the ones programs name: red, green, ...), the
    /// rest of the 256 left as they are.
    pub fn set_ansi(&mut self, colors: [Rgb; 16]) {
        let Ok(mut palette) = self.t.default_color_palette() else { return };
        for (i, c) in colors.iter().enumerate() {
            palette.0[i] = RgbColor { r: c.0, g: c.1, b: c.2 };
        }
        let _ = self.t.set_default_color_palette(Some(palette));
    }

    /// forget everything on it (before it's drawn again from a dump).
    pub fn reset(&mut self) {
        self.t.reset();
        self.anchor = None;
    }

    /// bytes from the program.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.t.vt_write(bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16, cell_w: u32, cell_h: u32) -> Result<()> {
        self.t.resize(cols.max(1), rows.max(1), cell_w, cell_h)?;
        Ok(())
    }

    pub fn cols(&self) -> u16 {
        self.t.cols().unwrap_or(0)
    }

    pub fn rows(&self) -> u16 {
        self.t.rows().unwrap_or(0)
    }

    /// what the program called itself (OSC 0/2), if anything.
    pub fn title(&self) -> String {
        self.t.title().map(str::to_owned).unwrap_or_default()
    }

    /// the folder the shell says it's in (OSC 7), if it says.
    pub fn pwd(&self) -> String {
        self.t.pwd().map(|p| p.strip_prefix("file://").map_or(p, |rest| rest.find('/').map_or(rest, |i| &rest[i..])).to_owned()).unwrap_or_default()
    }

    fn format(&self, opts: FormatterOptions<'_, '_>) -> Vec<u8> {
        let Ok(mut f) = Formatter::new(&self.t, opts) else { return Vec::new() };
        f.format_alloc(None).map(|b| b.to_vec()).unwrap_or_default()
    }

    /// the text on screen, or with `history` everything scrollback still holds.
    /// soft-wrapped lines are joined; trailing blanks are trimmed.
    pub fn text(&self, history: bool) -> String {
        let opts = FormatterOptions::new().with_format(Format::Plain).with_unwrap(true).with_trim(true);
        if history {
            return String::from_utf8_lossy(&self.format(opts)).trim_end_matches('\n').to_owned();
        }
        // just the screen: formatting all of history to keep its last rows
        // costs as much as the history is long, every time.
        let (cols, rows) = (self.cols(), self.rows());
        let a = self.t.grid_ref(Point::Active(PointCoordinate { x: 0, y: 0 }));
        let b = self.t.grid_ref(Point::Active(PointCoordinate { x: cols.saturating_sub(1), y: u32::from(rows.saturating_sub(1)) }));
        let (Ok(a), Ok(b)) = (a, b) else { return String::new() };
        let screen = Selection::new(a, b, false);
        String::from_utf8_lossy(&self.format(opts.with_selection(&screen))).trim_end_matches('\n').to_owned()
    }

    /// everything needed to draw this terminal again somewhere else, as the
    /// bytes a program would have sent: screen, history, colours, modes,
    /// cursor. a window that attaches late is fed this first.
    pub fn dump(&self) -> Vec<u8> {
        self.format(
            FormatterOptions::new()
                .with_format(Format::Vt)
                .with_palette(true)
                .with_modes(true)
                .with_scrolling_region(true)
                // not tab stops: they're written after the cursor, and setting
                // each one moves it (the cursor came back on the last stop). a
                // new terminal has the standard stops anyway.
                .with_pwd(true)
                .with_keyboard(true)
                .with_cursor(true)
                .with_style(true)
                .with_hyperlink(true)
                .with_kitty_keyboard(true)
                .with_charsets(true),
        )
    }

    pub fn mode(&self, mode: Mode) -> bool {
        self.t.mode(mode).unwrap_or(false)
    }

    /// the program wants the mouse (so a click isn't a selection).
    pub fn mouse_tracking(&self) -> bool {
        self.t.is_mouse_tracking().unwrap_or(false)
    }

    /// a full-screen program (vim, less) is on the alternate screen: no history.
    pub fn alternate(&self) -> bool {
        self.mode(Mode::ALT_SCREEN) || self.mode(Mode::ALT_SCREEN_SAVE) || self.mode(Mode::ALT_SCREEN_LEGACY)
    }

    /// move the view through history: up is negative.
    pub fn scroll(&mut self, rows: isize) {
        self.t.scroll_viewport(ScrollViewport::Delta(rows));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.t.scroll_viewport(ScrollViewport::Bottom);
    }

    /// a key, encoded as this program wants keys (cursor-key mode, kitty
    /// keyboard, ...). `text` is what the key typed, if anything.
    pub fn key(&mut self, key: Key, mods: Mods, text: Option<&str>, unshifted: char, action: KeyAction) -> Vec<u8> {
        let mut consumed = Mods::empty();
        if text.is_some() && unshifted != '\0' && mods.contains(Mods::SHIFT) {
            consumed |= Mods::SHIFT;
        }
        self.key_event.set_action(action).set_key(key).set_mods(mods).set_consumed_mods(consumed).set_unshifted_codepoint(unshifted).set_utf8(text);
        let mut out = Vec::new();
        let _ = self.keys.set_options_from_terminal(&self.t).encode_to_vec(&self.key_event, &mut out);
        // a key the encoder had nothing for, that still typed something.
        if out.is_empty()
            && let Some(t) = text
            && action != KeyAction::Release
        {
            out.extend_from_slice(t.as_bytes());
        }
        out
    }

    /// a mouse event for a program that asked for the mouse, encoded as it
    /// asked (x10, sgr, ...); nothing if it didn't ask. `x`, `y` are pixels
    /// from the grid's top left; `cell` its cell size; `held` whether any
    /// button is down (a drag, not a hover).
    #[allow(clippy::too_many_arguments)]
    pub fn mouse(&mut self, act: MouseAct, button: Option<MouseButton>, x: f32, y: f32, mods: Mods, held: bool, cell: (u32, u32)) -> Vec<u8> {
        use libghostty_vt::mouse::{Action, Button, EncoderSize, Position};
        if !self.mouse_tracking() {
            return Vec::new();
        }
        let (cols, rows) = (u32::from(self.cols()), u32::from(self.rows()));
        self.mouse
            .set_options_from_terminal(&self.t)
            .set_size(EncoderSize {
                screen_width: cols * cell.0,
                screen_height: rows * cell.1,
                cell_width: cell.0.max(1),
                cell_height: cell.1.max(1),
                padding_top: 0,
                padding_bottom: 0,
                padding_left: 0,
                padding_right: 0,
            })
            .set_any_button_pressed(held)
            .set_track_last_cell(true);
        let button = button.map(|b| match b {
            MouseButton::Left => Button::Left,
            MouseButton::Middle => Button::Middle,
            MouseButton::Right => Button::Right,
            MouseButton::WheelUp => Button::Four,
            MouseButton::WheelDown => Button::Five,
        });
        let action = match act {
            MouseAct::Press => Action::Press,
            MouseAct::Release => Action::Release,
            MouseAct::Motion => Action::Motion,
        };
        self.mouse_event.set_action(action).set_button(button).set_mods(mods).set_position(Position { x, y });
        let mut out = Vec::new();
        let _ = self.mouse.encode_to_vec(&self.mouse_event, &mut out);
        out
    }

    /// pasted text, made safe (control bytes become spaces) and bracketed if
    /// the program asked for bracketed paste.
    pub fn paste(&self, text: &str) -> Vec<u8> {
        let mut data = text.as_bytes().to_vec();
        let mut buf = vec![0u8; data.len() + 16];
        let bracketed = self.mode(Mode::BRACKETED_PASTE);
        match libghostty_vt::paste::encode(&mut data, bracketed, &mut buf) {
            Ok(n) => {
                buf.truncate(n);
                buf
            }
            Err(_) => Vec::new(),
        }
    }

    // ---- selection: cells are viewport columns and rows ----------------------

    fn screen_row(&self, viewport_row: u16) -> u32 {
        let sb = self.t.scrollbar().ok();
        // the viewport's first row, counted from the top of history.
        let top = sb.map_or(0, |s| s.offset as u32);
        top + u32::from(viewport_row)
    }

    /// start selecting at a cell.
    pub fn select_start(&mut self, col: u16, row: u16) {
        self.anchor = Some((col, self.screen_row(row)));
        let _ = self.t.set_selection(None);
    }

    /// extend the selection to a cell (while dragging).
    pub fn select_to(&mut self, col: u16, row: u16) {
        let Some((ax, ay)) = self.anchor else { return };
        let (bx, by) = (col.min(self.cols().saturating_sub(1)), self.screen_row(row));
        let a = self.t.grid_ref(Point::Screen(PointCoordinate { x: ax, y: ay }));
        let b = self.t.grid_ref(Point::Screen(PointCoordinate { x: bx, y: by }));
        if let (Ok(a), Ok(b)) = (a, b) {
            let sel = Selection::new(a, b, false);
            let _ = self.t.set_selection(Some(&sel));
        }
    }

    pub fn select_end(&mut self) {
        self.anchor = None;
    }

    pub fn clear_selection(&mut self) {
        self.anchor = None;
        let _ = self.t.set_selection(None);
    }

    /// the selected text, if there's a selection.
    pub fn selected_text(&self) -> Option<String> {
        let opts = libghostty_vt::selection::FormatOptions::new().with_emit_format(Format::Plain).with_unwrap(true).with_trim(true);
        let bytes = self.t.format_selection_alloc(None, opts).ok()??;
        let text = String::from_utf8_lossy(&bytes).into_owned();
        (!text.is_empty()).then_some(text)
    }

    // ---- drawing -----------------------------------------------------------------

    /// bring `screen` up to date. only rows that changed are rebuilt; the
    /// answer says whether anything did.
    pub fn update(&mut self, screen: &mut Screen) -> Result<bool> {
        let snap = self.render.update(&self.t)?;
        let dirty = snap.dirty()?;
        let rows = usize::from(snap.rows()?);
        let cols = snap.cols()?;
        let colors = snap.colors()?;
        let (fg, bg) = (Rgb::from(colors.foreground), Rgb::from(colors.background));
        let full = dirty == Dirty::Full || screen.lines.len() != rows || screen.cols != cols || screen.fg != fg || screen.bg != bg;
        if dirty == Dirty::Clean && !full {
            return Ok(false);
        }
        screen.cols = cols;
        screen.fg = fg;
        screen.bg = bg;
        screen.lines.resize_with(rows, Line::default);
        screen.cursor = match (snap.cursor_visible()?, snap.cursor_viewport()?) {
            (true, Some(c)) => Some(Cursor {
                col: c.x,
                row: c.y,
                blink: snap.cursor_blinking().unwrap_or(false),
                shape: match snap.cursor_visual_style() {
                    Ok(libghostty_vt::render::CursorVisualStyle::Bar) => CursorShape::Bar,
                    Ok(libghostty_vt::render::CursorVisualStyle::Underline) => CursorShape::Underline,
                    Ok(libghostty_vt::render::CursorVisualStyle::BlockHollow) => CursorShape::Hollow,
                    _ => CursorShape::Block,
                },
            }),
            _ => None,
        };
        let mut rows_it = self.rows.update(&snap)?;
        let mut y = 0usize;
        let mut g = String::with_capacity(8);
        while let Some(row) = rows_it.next() {
            if y >= rows {
                break;
            }
            if full || row.dirty().unwrap_or(true) {
                let line = &mut screen.lines[y];
                line.text.clear();
                line.spans.clear();
                screen.next_version += 1;
                line.version = screen.next_version;
                let mut cells = self.cells.update(row)?;
                let mut x: u16 = 0;
                let mut last_wide = false;
                while let Some(cell) = cells.next() {
                    let wide = cell.raw_cell().and_then(|c| c.wide()).unwrap_or(CellWide::Narrow);
                    match wide {
                        // the second half of a wide character: counted with it.
                        CellWide::SpacerTail => continue,
                        // a wide character that didn't fit at a line's end: an empty cell.
                        CellWide::SpacerHead => {
                            x += 1;
                            continue;
                        }
                        _ => {}
                    }
                    let style = cell.style().unwrap_or_default();
                    let selected = cell.is_selected().unwrap_or(false);
                    let mut cfg = cell.fg_color().ok().flatten().map_or(fg, Rgb::from);
                    let mut cbg = cell.bg_color().ok().flatten().map(Rgb::from);
                    if style.inverse != selected {
                        let back = cbg.unwrap_or(bg);
                        cbg = Some(cfg);
                        cfg = back;
                    }
                    if style.invisible {
                        cfg = cbg.unwrap_or(bg);
                    }
                    g.clear();
                    if cell.graphemes_len().unwrap_or(0) == 0 || cell.graphemes_utf8(&mut g).is_err() {
                        g.clear();
                        g.push(' ');
                    }
                    let width: u16 = if wide == CellWide::Wide { 2 } else { 1 };
                    let start = line.text.len();
                    line.text.push_str(&g);
                    let underline = !matches!(style.underline, libghostty_vt::style::Underline::None);
                    // a wide character never shares a run, either side of it.
                    let joins = |s: &Span| {
                        width == 1
                            && !last_wide
                            && s.col + s.cols == x
                            && s.fg == cfg
                            && s.bg == cbg
                            && s.bold == style.bold
                            && s.italic == style.italic
                            && s.faint == style.faint
                            && s.underline == underline
                            && s.strike == style.strikethrough
                    };
                    match line.spans.last_mut() {
                        Some(s) if joins(s) => {
                            s.cols += 1;
                            s.end = line.text.len();
                        }
                        _ => line.spans.push(Span {
                            col: x,
                            cols: width,
                            start,
                            end: line.text.len(),
                            fg: cfg,
                            bg: cbg,
                            bold: style.bold,
                            italic: style.italic,
                            faint: style.faint,
                            underline,
                            strike: style.strikethrough,
                        }),
                    }
                    last_wide = width == 2;
                    x += width;
                }
                // trailing blanks on the terminal's own background draw nothing.
                while let Some(s) = line.spans.last_mut() {
                    if s.bg.is_some() || s.underline || s.strike || s.cols as usize != line.text[s.start..s.end].chars().count() {
                        break;
                    }
                    let kept = line.text[s.start..s.end].trim_end_matches(' ').len();
                    let blanks = (s.end - s.start - kept) as u16;
                    s.end = s.start + kept;
                    s.cols -= blanks;
                    line.text.truncate(s.end);
                    if s.cols == 0 {
                        line.spans.pop();
                    } else {
                        break;
                    }
                }
                let _ = row.set_dirty(false);
            }
            y += 1;
        }
        let _ = snap.set_dirty(Dirty::Clean);
        if let Ok(sb) = self.t.scrollbar() {
            screen.history = (sb.total as usize).saturating_sub(sb.len as usize);
            screen.scrolled = screen.history.saturating_sub(sb.offset as usize);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen_of(bytes: &[u8]) -> (Term, Screen) {
        let mut t = Term::new(20, 4, 100).unwrap();
        t.feed(bytes);
        let mut s = Screen::default();
        t.update(&mut s).unwrap();
        (t, s)
    }

    #[test]
    fn text_and_colours_come_out_as_runs() {
        let (_, s) = screen_of(b"hi \x1b[31mred\x1b[0m!");
        assert_eq!(s.lines[0].text, "hi red!");
        let red = s.lines[0].spans.iter().find(|sp| &s.lines[0].text[sp.start..sp.end] == "red").expect("its own run");
        assert_eq!(red.col, 3);
        assert_ne!(red.fg, s.fg, "drawn in red, not the default");
        assert!(s.lines[1].text.is_empty(), "blank rows draw nothing");
    }

    #[test]
    fn a_wide_character_is_a_run_of_its_own_two_cells_wide() {
        let (_, s) = screen_of("a界b".as_bytes());
        let spans: Vec<(u16, u16, &str)> = s.lines[0].spans.iter().map(|sp| (sp.col, sp.cols, &s.lines[0].text[sp.start..sp.end])).collect();
        assert_eq!(spans, [(0, 1, "a"), (1, 2, "界"), (3, 1, "b")]);
    }

    #[test]
    fn only_changed_rows_are_rebuilt() {
        let (mut t, mut s) = screen_of(b"one\r\ntwo");
        let (v0, v1) = (s.lines[0].version, s.lines[1].version);
        t.feed(b"!");
        assert!(t.update(&mut s).unwrap());
        assert_eq!(s.lines[0].version, v0, "untouched");
        assert_ne!(s.lines[1].version, v1);
        assert_eq!(s.lines[1].text, "two!");
        assert!(!t.update(&mut s).unwrap(), "nothing new: nothing to do");
    }

    #[test]
    fn text_is_the_screen_or_the_history_too() {
        let mut t = Term::new(10, 2, 100).unwrap();
        t.feed(b"1\r\n2\r\n3\r\n4");
        assert_eq!(t.text(false), "3\n4");
        assert_eq!(t.text(true), "1\n2\n3\n4");
    }

    #[test]
    fn a_dump_fed_to_a_new_terminal_draws_the_same_screen() {
        let (t, s) = screen_of(b"\x1b[1;32mgreen\x1b[0m\r\nplain");
        let mut copy = Term::new(20, 4, 100).unwrap();
        copy.feed(&t.dump());
        let mut s2 = Screen::default();
        copy.update(&mut s2).unwrap();
        assert_eq!(s2.lines[0].text, s.lines[0].text);
        assert_eq!(s2.lines[0].spans[0].fg, s.lines[0].spans[0].fg);
        assert!(s2.lines[0].spans[0].bold);
        assert_eq!(s2.lines[1].text, "plain");
    }

    #[test]
    fn the_program_s_questions_are_answered_through_the_callback() {
        let out = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let mut t = Term::new(20, 4, 100).unwrap();
        let o = out.clone();
        t.answer_with(move |b| o.borrow_mut().extend_from_slice(b)).unwrap();
        t.feed(b"\x1b[c"); // primary device attributes
        assert!(out.borrow().starts_with(b"\x1b[?"), "{:?}", String::from_utf8_lossy(&out.borrow()));
    }

    #[test]
    fn keys_follow_the_program_s_modes() {
        let mut t = Term::new(20, 4, 100).unwrap();
        assert_eq!(t.key(Key::ArrowUp, Mods::empty(), None, '\0', KeyAction::Press), b"\x1b[A");
        t.feed(b"\x1b[?1h"); // application cursor keys
        assert_eq!(t.key(Key::ArrowUp, Mods::empty(), None, '\0', KeyAction::Press), b"\x1bOA");
        assert_eq!(t.key(Key::A, Mods::empty(), Some("a"), 'a', KeyAction::Press), b"a");
        assert_eq!(t.key(Key::C, Mods::CTRL, None, 'c', KeyAction::Press), b"\x03");
    }

    #[test]
    fn a_paste_is_bracketed_only_when_asked_for() {
        let mut t = Term::new(20, 4, 100).unwrap();
        assert_eq!(t.paste("a\nb"), b"a\rb");
        t.feed(b"\x1b[?2004h");
        assert_eq!(t.paste("a\nb"), b"\x1b[200~a\nb\x1b[201~");
    }

    #[test]
    fn a_drag_selects_text_across_rows() {
        let (mut t, _) = screen_of(b"hello world\r\nsecond line");
        t.select_start(6, 0);
        t.select_to(5, 1);
        assert_eq!(t.selected_text().as_deref(), Some("world\nsecond"));
        t.clear_selection();
        assert_eq!(t.selected_text(), None);
    }
}

#[cfg(test)]
mod cursor_after_dump {
    use super::*;

    fn through_a_dump(bytes: &[u8]) -> (Option<(u16, u16)>, Option<(u16, u16)>) {
        let mut t = Term::new(40, 10, 100).unwrap();
        t.feed(bytes);
        let mut a = Screen::default();
        t.update(&mut a).unwrap();
        let mut c = Term::new(40, 10, 100).unwrap();
        c.feed(&t.dump());
        let mut b = Screen::default();
        c.update(&mut b).unwrap();
        (a.cursor.map(|c| (c.col, c.row)), b.cursor.map(|c| (c.col, c.row)))
    }

    #[test]
    fn a_window_that_attaches_late_has_the_cursor_where_it_was() {
        let history: Vec<u8> = (0..40).map(|i| format!("line {i}\r\n")).collect::<String>().into_bytes().into_iter().chain(b"> ".iter().copied()).collect();
        for bytes in [b"abc\r\nde".to_vec(), history, b"\x1b[1;1Hleft\x1b[1;30Hright\x1b[1;6H".to_vec()] {
            let (before, after) = through_a_dump(&bytes);
            assert_eq!(before, after, "{:?}", String::from_utf8_lossy(&bytes));
        }
    }
}

#[cfg(test)]
mod speed {
    use super::*;
    use std::time::Instant;

    /// run with --ignored --nocapture: how fast ghostty takes bytes, and what
    /// reading the screen costs once history is full.
    #[test]
    #[ignore]
    fn throughput() {
        let line = b"the quick brown fox jumps over the lazy dog 0123456789 ABCDEFGHIJKLMNOP\r\n";
        let chunk: Vec<u8> = line.iter().copied().cycle().take(4096).collect();
        let mut t = Term::new(120, 40, 10_000).unwrap();
        let n = 100_000_000 / chunk.len();
        let start = Instant::now();
        for _ in 0..n {
            t.feed(&chunk);
        }
        let secs = start.elapsed().as_secs_f64();
        println!("feed: {:.0} MB/s", 100.0 / secs);
        let start = Instant::now();
        for _ in 0..1000 {
            std::hint::black_box(t.text(false));
        }
        println!("text(screen): {:.1} us", start.elapsed().as_secs_f64() * 1e3);
        let start = Instant::now();
        std::hint::black_box(t.text(true));
        println!("text(history): {:.1} ms", start.elapsed().as_secs_f64() * 1e3);
        let mut s = Screen::default();
        let start = Instant::now();
        for _ in 0..1000 {
            t.feed(&chunk);
            t.update(&mut s).unwrap();
        }
        println!("feed 4k + update: {:.1} us", start.elapsed().as_secs_f64() * 1e3);
    }
}

#[cfg(test)]
mod ansi {
    use super::*;

    #[test]
    fn a_program_s_red_is_the_red_it_was_given() {
        let mut t = Term::new(20, 4, 10).unwrap();
        let mut colors = [Rgb(0, 0, 0); 16];
        colors[1] = Rgb(200, 10, 20);
        t.set_ansi(colors);
        t.feed(b"\x1b[31mred");
        let mut s = Screen::default();
        t.update(&mut s).unwrap();
        assert_eq!(s.lines[0].spans[0].fg, Rgb(200, 10, 20));
    }
}

#[cfg(test)]
mod mouse {
    use super::*;

    #[test]
    fn a_click_reaches_a_program_only_when_it_asked_and_in_its_format() {
        let mut t = Term::new(80, 24, 10).unwrap();
        assert!(t.mouse(MouseAct::Press, Some(MouseButton::Left), 25.0, 10.0, Mods::empty(), true, (10, 20)).is_empty(), "nobody asked");
        t.feed(b"\x1b[?1000h\x1b[?1006h"); // clicks, sgr format
        let press = t.mouse(MouseAct::Press, Some(MouseButton::Left), 25.0, 10.0, Mods::empty(), true, (10, 20));
        assert_eq!(press, b"\x1b[<0;3;1M", "column 3, row 1, 1-based");
        let wheel = t.mouse(MouseAct::Press, Some(MouseButton::WheelUp), 25.0, 10.0, Mods::empty(), false, (10, 20));
        assert_eq!(wheel, b"\x1b[<64;3;1M");
    }
}
