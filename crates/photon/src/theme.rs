//! the design's fixed vocabulary: colour, type, spacing, sizes.
//!
//! two rules shape the palette:
//! - colour means "this wants you". orange is blocked on you, red is failed,
//!   green is finished and unseen. nothing else in the app is coloured.
//! - everything has a light and a dark value, and the window follows the system.
//!
//! values come from apple's published system colours where apple publishes
//! them. apple does not publish its semantic colours (label, separator, window
//! background), so those are the long-standing measured values; reading them
//! live from macos is a later step.

use gpui_kit::component::{Theme, ThemeMode};
use crate::ui::{ThemeSpec, Token};
use gpui_kit::{App, FontWeight, Hsla, Pixels, SharedString, Styled, Window, WindowAppearance, px, rgb};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub dark: bool,
    /// the content area.
    pub window: Hsla,
    pub sidebar: Hsla,
    /// one-point lines between regions.
    pub separator: Hsla,
    /// text, in four strengths.
    pub label: Hsla,
    pub secondary: Hsla,
    pub tertiary: Hsla,
    pub quaternary: Hsla,
    /// the selected row.
    pub selection: Hsla,
    pub hover: Hsla,
    /// a quiet fill for things that sit on the content area: your own messages, fields.
    pub fill: Hsla,
    /// surfaces that float above the content: cards, a message box drawn as one.
    pub raised: Hsla,
    /// selection and focus. the system accent.
    pub accent: Hsla,
    pub on_accent: Hsla,
    // the only colours with meaning. each is "wants you", for a different reason.
    pub blocked: Hsla,
    pub failed: Hsla,
    pub finished: Hsla,
}

fn ink(hex: u32, alpha: f32) -> Hsla {
    let mut c: Hsla = rgb(hex).into();
    c.a = alpha;
    c
}

impl Palette {
    pub fn light() -> Self {
        Self {
            dark: false,
            window: ink(0xffffff, 1.0),
            sidebar: ink(0xf4f4f5, 1.0),
            separator: ink(0x000000, 0.10),
            label: ink(0x000000, 0.85),
            // darker than the 50% macos itself uses: at 50% small text is
            // 3.98:1 on white, under apple's own 4.5:1 floor.
            secondary: ink(0x000000, 0.60),
            tertiary: ink(0x000000, 0.26),
            quaternary: ink(0x000000, 0.10),
            selection: ink(0x000000, 0.09),
            hover: ink(0x000000, 0.045),
            fill: ink(0x000000, 0.05),
            raised: ink(0xffffff, 1.0),
            // apple's published system colours, light values.
            accent: ink(0x0088ff, 1.0),
            on_accent: ink(0xffffff, 1.0),
            blocked: ink(0xff8d28, 1.0),
            failed: ink(0xff383c, 1.0),
            finished: ink(0x34c759, 1.0),
        }
    }

    pub fn dark() -> Self {
        Self {
            dark: true,
            window: ink(0x1e1e1e, 1.0),
            sidebar: ink(0x252526, 1.0),
            separator: ink(0xffffff, 0.10),
            label: ink(0xffffff, 0.85),
            secondary: ink(0xffffff, 0.55),
            tertiary: ink(0xffffff, 0.25),
            quaternary: ink(0xffffff, 0.10),
            selection: ink(0xffffff, 0.11),
            hover: ink(0xffffff, 0.055),
            fill: ink(0xffffff, 0.07),
            raised: ink(0x2a2a2b, 1.0),
            // apple's published system colours, dark values.
            accent: ink(0x0091ff, 1.0),
            on_accent: ink(0xffffff, 1.0),
            blocked: ink(0xff9230, 1.0),
            failed: ink(0xff4245, 1.0),
            finished: ink(0x30d158, 1.0),
        }
    }

    /// hack club orchard's default palette ("orchard" preset, its generated
    /// scales): a warm tinted neutral and one red accent. roles map the way
    /// orchard maps them: app is its chrome, sheet its surface, steps 10, 9
    /// and 8 its three text tones.
    pub fn orchard_light() -> Self {
        Self {
            dark: false,
            window: ink(0xfffafa, 1.0),
            sidebar: ink(0xf7eeee, 1.0),
            separator: ink(0xeddedd, 1.0),
            label: ink(0x221e1e, 1.0),
            secondary: ink(0x594f50, 1.0),
            tertiary: ink(0x877c7c, 1.0),
            quaternary: ink(0xd8cbcb, 1.0),
            selection: ink(0xeddfdf, 1.0),
            hover: ink(0xf2e6e6, 1.0),
            fill: ink(0xf7eded, 1.0),
            raised: ink(0xfffbfb, 1.0),
            accent: ink(0xdf3047, 1.0),
            on_accent: ink(0xffffff, 1.0),
            blocked: ink(0xf6ae36, 1.0),
            failed: ink(0xe42933, 1.0),
            finished: ink(0x279753, 1.0),
        }
    }

    pub fn orchard_dark() -> Self {
        Self {
            dark: true,
            window: ink(0x1c1111, 1.0),
            sidebar: ink(0x180c0d, 1.0),
            separator: ink(0x2f2222, 1.0),
            label: ink(0xf2ecec, 1.0),
            secondary: ink(0xa19594, 1.0),
            tertiary: ink(0x8f8081, 1.0),
            quaternary: ink(0x473839, 1.0),
            selection: ink(0x322425, 1.0),
            hover: ink(0x2a1d1e, 1.0),
            fill: ink(0x231819, 1.0),
            raised: ink(0x241919, 1.0),
            accent: ink(0xeb4b58, 1.0),
            on_accent: ink(0x1b1414, 1.0),
            blocked: ink(0xf2af48, 1.0),
            failed: ink(0xee4848, 1.0),
            finished: ink(0x3aa460, 1.0),
        }
    }

    /// tern's palette (stencil's terminal), from its own stylesheet: a warm
    /// grey page, a near-white panel, inks as one colour at four strengths,
    /// hairlines as that ink at a few percent.
    pub fn tern_light() -> Self {
        Self {
            dark: false,
            window: ink(0xfbfbfa, 1.0),
            sidebar: ink(0xefefec, 1.0),
            separator: ink(0x181a2c, 0.09),
            label: ink(0x1b1c20, 1.0),
            secondary: ink(0x1b1c20, 0.66),
            tertiary: ink(0x1b1c20, 0.44),
            quaternary: ink(0x1b1c20, 0.14),
            selection: ink(0x181a2c, 0.09),
            hover: ink(0x181a2c, 0.06),
            fill: ink(0x000000, 0.035),
            raised: ink(0xffffff, 1.0),
            accent: ink(0x3d6fff, 1.0),
            on_accent: ink(0xffffff, 1.0),
            blocked: ink(0xd98a0b, 1.0),
            failed: ink(0xe5484d, 1.0),
            finished: ink(0x2fae74, 1.0),
        }
    }

    pub fn tern_dark() -> Self {
        Self {
            dark: true,
            window: ink(0x0f0f12, 1.0),
            sidebar: ink(0x08080a, 1.0),
            separator: ink(0xffffff, 0.1),
            label: ink(0xededed, 1.0),
            secondary: ink(0xededed, 0.7),
            tertiary: ink(0xededed, 0.42),
            quaternary: ink(0xededed, 0.14),
            selection: ink(0xffffff, 0.1),
            hover: ink(0xffffff, 0.07),
            fill: ink(0xffffff, 0.03),
            raised: ink(0x161619, 1.0),
            accent: ink(0x5a8cff, 1.0),
            on_accent: ink(0xffffff, 1.0),
            blocked: ink(0xf5a524, 1.0),
            failed: ink(0xff6166, 1.0),
            finished: ink(0x3ecf8e, 1.0),
        }
    }

    /// a colour by the role name layouts use.
    pub fn get(&self, token: Token) -> Hsla {
        match token {
            Token::App => self.sidebar,
            Token::Sheet => self.window,
            Token::Raised => self.raised,
            Token::Sunken => self.fill,
            Token::Hover => self.hover,
            Token::Active => self.selection,
            Token::Text => self.label,
            Token::Muted => self.secondary,
            Token::Faint => self.tertiary,
            Token::Ghost => self.quaternary,
            Token::Border => self.separator,
            Token::Accent => self.accent,
            Token::OnAccent => self.on_accent,
            Token::Warning => self.blocked,
            Token::Error => self.failed,
            Token::Success => self.finished,
        }
    }

    pub fn set(&mut self, token: Token, c: Hsla) {
        let slot = match token {
            Token::App => &mut self.sidebar,
            Token::Sheet => &mut self.window,
            Token::Raised => &mut self.raised,
            Token::Sunken => &mut self.fill,
            Token::Hover => &mut self.hover,
            Token::Active => &mut self.selection,
            Token::Text => &mut self.label,
            Token::Muted => &mut self.secondary,
            Token::Faint => &mut self.tertiary,
            Token::Ghost => &mut self.quaternary,
            Token::Border => &mut self.separator,
            Token::Accent => &mut self.accent,
            Token::OnAccent => &mut self.on_accent,
            Token::Warning => &mut self.blocked,
            Token::Error => &mut self.failed,
            Token::Success => &mut self.finished,
        };
        *slot = c;
    }

    /// make a theme's colours readable: text and secondary text at 4.5:1 on
    /// every surface, the accent visible on the content, text on the accent
    /// readable, hairlines visible. colours that already are stay exactly as
    /// they are, so photon's and orchard's own palettes pass through untouched.
    pub fn keep_readable(&mut self) {
        for _ in 0..3 {
            for bg in [self.window, self.sidebar, self.raised] {
                self.label = readable(self.label, bg, 4.5);
                self.secondary = readable(self.secondary, bg, 4.5);
            }
        }
        self.accent = readable(self.accent, self.window, 3.0);
        if contrast(self.on_accent, self.accent) < 3.0 {
            self.on_accent = best_ink(self.accent);
        }
        self.separator = readable(self.separator, self.window, 1.1);
    }

    /// whether a colour is one of the surfaces the palette was made for.
    pub fn is_surface(&self, c: Hsla) -> bool {
        c.a == 0.0 || [self.window, self.sidebar, self.raised, self.fill, self.hover, self.selection].contains(&c)
    }

    /// the palette for things drawn on `bg`, a background the theme didn't
    /// plan for. if its text is still readable there nothing changes but the
    /// surface; if not, every ink is re-derived from black or white,
    /// whichever reads on it, at the same strengths the theme uses.
    pub fn on(&self, bg: Hsla) -> Palette {
        let bg = if bg.a >= 1.0 { bg } else { over(bg, self.window) };
        let mut p = *self;
        p.window = bg;
        if contrast(self.label, bg) >= 4.5 && contrast(self.secondary, bg) >= 4.5 {
            return p;
        }
        let ink = best_ink(bg);
        let at = |a: f32| {
            let mut c = ink;
            c.a = a;
            c
        };
        p.dark = luminance(bg, bg) < 0.4;
        p.label = readable(at(0.88), bg, 4.5);
        p.secondary = readable(at(0.66), bg, 4.5);
        p.tertiary = at(0.4);
        p.quaternary = at(0.14);
        p.separator = at(0.14);
        p.fill = at(0.07);
        p.hover = at(0.06);
        p.selection = at(0.11);
        p.raised = over(at(0.05), bg);
        p.sidebar = bg;
        p.accent = readable(self.accent, bg, 3.0);
        if contrast(p.on_accent, p.accent) < 3.0 {
            p.on_accent = best_ink(p.accent);
        }
        p
    }

    pub fn for_appearance(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Self::dark(),
            _ => Self::light(),
        }
    }
}

/// wcag relative luminance of a colour composited over a background.
pub fn luminance(c: Hsla, under: Hsla) -> f32 {
    let (c, o) = (gpui_kit::Rgba::from(c), gpui_kit::Rgba::from(under));
    let mix = |fg: f32, bg: f32| fg * c.a + bg * (1.0 - c.a);
    let lin = |v: f32| if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) };
    0.2126 * lin(mix(c.r, o.r)) + 0.7152 * lin(mix(c.g, o.g)) + 0.0722 * lin(mix(c.b, o.b))
}

/// the wcag contrast ratio of a colour drawn on a background.
pub fn contrast(fg: Hsla, bg: Hsla) -> f32 {
    let (a, b) = (luminance(fg, bg), luminance(bg, bg));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// a colour composited over an opaque one: what is actually seen.
pub fn over(c: Hsla, under: Hsla) -> Hsla {
    let (f, b) = (gpui_kit::Rgba::from(c), gpui_kit::Rgba::from(under));
    let mix = |x: f32, y: f32| x * f.a + y * (1.0 - f.a);
    gpui_kit::Rgba { r: mix(f.r, b.r), g: mix(f.g, b.g), b: mix(f.b, b.b), a: 1.0 }.into()
}

/// black or white, whichever reads better on `bg`. (not pure: nearly black
/// and nearly white, as apple's label colours are.)
pub fn best_ink(bg: Hsla) -> Hsla {
    let (black, white) = (ink(0x1d1d1f, 1.0), ink(0xffffff, 1.0));
    if contrast(black, bg) >= contrast(white, bg) { black } else { white }
}

/// `fg` if it reads at `target` on `bg`; otherwise the nearest colour of its
/// hue that does, moved toward black on light backgrounds and white on dark.
pub fn readable(fg: Hsla, bg: Hsla, target: f32) -> Hsla {
    if contrast(fg, bg) >= target {
        return fg;
    }
    let toward_dark = luminance(bg, bg) > 0.18;
    let mut c = over(fg, bg);
    for _ in 0..50 {
        c.l = if toward_dark { (c.l - 0.02).max(0.0) } else { (c.l + 0.02).min(1.0) };
        if contrast(c, bg) >= target {
            return c;
        }
    }
    best_ink(bg)
}

/// pick light or dark (a forced choice, then the theme's, then the system's),
/// build the theme's palette for it, and make the library's own controls
/// (buttons, fields) agree with it and with mac sizing.
pub fn apply(force_dark: Option<bool>, spec: &ThemeSpec, window: &mut Window, cx: &mut App) -> Palette {
    let system_dark = matches!(window.appearance(), WindowAppearance::Dark | WindowAppearance::VibrantDark);
    let dark = force_dark.or(spec.dark.flatten()).unwrap_or(system_dark);
    let palette = spec.palette(dark);
    set_mono(spec.mono.as_deref().unwrap_or(MONO));
    set_scale(spec.scale.unwrap_or(1.0));
    // the library's own controls are sized in rems.
    window.set_rem_size(px(16.0 * scale()));
    let mode = if palette.dark { ThemeMode::Dark } else { ThemeMode::Light };
    Theme::change(mode, Some(window), cx);
    Theme::update(cx, |t| {
        t.font_size = z(13.);
        t.radius = px(R_CONTROL);
        t.radius_lg = px(R_FLOAT);
        t.colors.background = palette.window;
        t.colors.foreground = palette.label;
        t.colors.muted_foreground = palette.secondary;
        t.colors.border = palette.separator;
        t.colors.primary = palette.accent;
        t.colors.primary_hover = palette.accent;
        t.colors.primary_active = palette.accent;
        t.colors.primary_foreground = palette.on_accent;
        // the library colours its main button separately from "primary".
        t.colors.button_primary = palette.accent;
        t.colors.button_primary_hover = palette.accent.opacity(0.9);
        t.colors.button_primary_active = palette.accent.opacity(0.8);
        t.colors.button_primary_foreground = palette.on_accent;
        t.mono_font_family = mono();
        // a mac scrollbar is there when you reach for it, not all the time.
        t.scrollbar_mode = gpui_kit::component::scroll::ScrollbarMode::Hover;
        if let Some(font) = &spec.font {
            t.font_family = font.clone().into();
        }
    });
    palette
}

/// how the agent's markdown is set: calm headings, code in the mono face on
/// a sunken fill, tables in hairlines. one style for every place markdown
/// appears, so a reply reads the same in the transcript, a plan, an artifact.
pub fn prose(p: &Palette) -> gpui_kit::component::text::TextViewStyle {
    use gpui_kit::component::highlighter::HighlightTheme;
    use gpui_kit::component::text::TextViewStyle;
    use gpui_kit::{HighlightStyle, StyleRefinement, rems};
    let mut code = StyleRefinement::default().rounded(px(R_CARD)).bg(p.fill).border_1().border_color(p.separator).px(px(14.)).py(px(10.)).text_size(px(12.)).line_height(px(18.));
    code.overflow.x = Some(gpui_kit::Overflow::Scroll);
    let mut style = TextViewStyle::default()
        .paragraph_gap(rems(0.7))
        // a heading is a step up, not a shout: 17, 15, then body size.
        .heading_font_size(|level, _| px(match level {
            1 => 17.,
            2 => 15.,
            _ => 14.,
        }))
        .code_block(code)
        .table(StyleRefinement::default().rounded(px(R_CARD)).border_1().border_color(p.separator))
        .table_head(StyleRefinement::default().bg(p.fill).text_color(p.secondary).text_size(px(12.)))
        .table_cell(StyleRefinement::default().px(px(10.)).py(px(5.)).text_size(px(13.)))
        .inline_code(HighlightStyle { background_color: Some(p.fill), ..HighlightStyle::default() });
    style.highlight_theme = if p.dark { HighlightTheme::default_dark() } else { HighlightTheme::default_light() };
    style.is_dark = p.dark;
    style
}

/// apple's mac text styles (size / line height), plus two of our own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    /// 15/20 semibold. the open session's name.
    Title,
    /// 13/16 semibold.
    Headline,
    /// 13/16. the default for controls and rows.
    Body,
    /// 12/15.
    Callout,
    /// 11/14. second lines, section headers, times.
    Subheadline,
    /// 14/21. ours: agent prose. apple's 13/16 is tuned for controls, and is
    /// too tight for paragraphs.
    Prose,
    /// 12/17 monospace. ours: commands and tool rows.
    Code,
}

impl Type {
    pub fn metrics(self) -> (Pixels, Pixels, FontWeight) {
        let (size, line, weight) = self.base();
        (z(size), z(line), weight)
    }

    /// at the default scale.
    pub fn base(self) -> (f32, f32, FontWeight) {
        match self {
            Type::Title => (15., 20., FontWeight::SEMIBOLD),
            Type::Headline => (13., 16., FontWeight::SEMIBOLD),
            Type::Body => (13., 16., FontWeight::NORMAL),
            Type::Callout => (12., 15., FontWeight::NORMAL),
            Type::Subheadline => (11., 14., FontWeight::NORMAL),
            Type::Prose => (14., 21., FontWeight::NORMAL),
            Type::Code => (12., 17., FontWeight::NORMAL),
        }
    }
}

/// the monospace face unless a theme names another. menlo ships with every
/// mac. (apple's sf mono is not installed under a name apps can ask for.)
pub const MONO: &str = "Menlo";

static MONO_FACE: std::sync::LazyLock<std::sync::RwLock<SharedString>> = std::sync::LazyLock::new(|| std::sync::RwLock::new(SharedString::new_static(MONO)));

/// how big the interface is drawn: 1 is the default. text and the parts
/// built from it (menus, rows, controls) are multiplied by it.
static SCALE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0x3f80_0000);

pub fn scale() -> f32 {
    f32::from_bits(SCALE.load(std::sync::atomic::Ordering::Relaxed))
}

fn set_scale(v: f32) {
    SCALE.store(v.clamp(0.75, 1.5).to_bits(), std::sync::atomic::Ordering::Relaxed);
}

/// points at the interface's scale.
pub fn z(v: f32) -> Pixels {
    px((v * scale() * 2.0).round() / 2.0)
}

/// the named sizes the settings offer.
pub const SCALES: [(&str, f32); 3] = [("compact", 0.9), ("default", 1.0), ("large", 1.12)];

/// the monospace face in use.
pub fn mono() -> SharedString {
    MONO_FACE.read().map(|m| m.clone()).unwrap_or_else(|_| SharedString::new_static(MONO))
}

fn set_mono(face: &str) {
    if let Ok(mut m) = MONO_FACE.write()
        && m.as_ref() != face
    {
        *m = SharedString::from(face.to_owned());
    }
}

pub trait Typeset: Styled + Sized {
    /// set text in one of the named styles.
    fn set(self, style: Type) -> Self {
        let (size, line, weight) = style.metrics();
        let this = self.text_size(size).line_height(line).font_weight(weight);
        if style == Type::Code { this.font_family(mono()) } else { this }
    }
}

impl<T: Styled + Sized> Typeset for T {}

// ---- corners: orchard's scale ---------------------------------------------------
//
// containers 18, cards 14, controls 10, inner tiles 8. never one radius
// everywhere, and never equal radii nested: something inside a rounded thing
// is rounded by the outer radius less the room between them.

/// sheets, panes, floating menus.
pub const R_PANEL: f32 = 18.0;
/// cards: the message box, prompt cards, code blocks, tables, bubbles.
pub const R_CARD: f32 = 14.0;
/// controls: buttons, fields, segmented trays.
pub const R_CONTROL: f32 = 10.0;
/// inner tiles: rows in a list, chips, segments in a tray.
pub const R_TILE: f32 = 8.0;
/// a floating menu's inset; its items are rounded `R_FLOAT - FLOAT_PAD`.
pub const FLOAT_PAD: f32 = 5.0;
/// a floating menu's corner: smaller than a pane's, as tern's are.
pub const R_FLOAT: f32 = 14.0;
/// a floating menu item: tern's 34.
pub const FLOAT_ITEM_H: f32 = 34.0;

/// the inside of a floating menu: inset so its items sit concentric in its corner.
pub fn float_list(_p: &Palette) -> gpui_kit::Div {
    // a menu never takes the weight or size of what opened it.
    gpui_kit::component::v_flex().p(z(FLOAT_PAD)).gap(px(1.)).font_weight(FontWeight::NORMAL).text_size(z(13.5))
}

/// a shortcut as keycaps: "⇧ ⌘ D" is three keys.
pub fn keycaps(p: &Palette, keys: &str) -> gpui_kit::Div {
    use gpui_kit::{ParentElement as _, div};
    let mut row = gpui_kit::component::h_flex().flex_shrink_0().gap(px(2.)).font_weight(FontWeight::NORMAL);
    for k in keys.split_whitespace() {
        row = row.child(
            div()
                .min_w(z(18.))
                .h(z(18.))
                .px(z(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(5.))
                .border_1()
                .border_b_2()
                .border_color(p.separator)
                .bg(p.raised)
                .text_size(z(10.5))
                .line_height(z(10.5))
                .font_weight(FontWeight::MEDIUM)
                .text_color(p.tertiary)
                .child(k.to_owned()),
        );
    }
    row
}

/// one choice in a floating menu: an icon, a name, a quiet note, and a check
/// when it is the current one. everything stays inside the highlight.
pub fn float_item(p: &Palette, id: impl Into<SharedString>, icon: Option<&str>, name: impl Into<SharedString>, note: Option<&str>, current: bool) -> gpui_kit::Stateful<gpui_kit::Div> {
    float_item_with(p, id, icon, name, note, None, current)
}

/// a floating menu item with its shortcut shown at the end.
pub fn float_item_with(p: &Palette, id: impl Into<SharedString>, icon: Option<&str>, name: impl Into<SharedString>, note: Option<&str>, keys: Option<&str>, current: bool) -> gpui_kit::Stateful<gpui_kit::Div> {
    float_row(p, id, icon, name.into(), note, keys, current, None)
}

/// a menu item that destroys something: red, with a red wash under the pointer.
pub fn float_danger(p: &Palette, id: impl Into<SharedString>, icon: Option<&str>, name: impl Into<SharedString>, note: Option<&str>) -> gpui_kit::Stateful<gpui_kit::Div> {
    float_row(p, id, icon, name.into(), note, None, false, Some(p.failed))
}

/// a menu item, sized as tern sizes them: 34 tall (46 with a description),
/// 10 in from the sides and between icon and label, 13.5 text, a 15 point
/// icon in the muted ink, and a corner concentric with the menu's.
#[allow(clippy::too_many_arguments)]
fn float_row(p: &Palette, id: impl Into<SharedString>, icon: Option<&str>, name: SharedString, note: Option<&str>, keys: Option<&str>, current: bool, danger: Option<Hsla>) -> gpui_kit::Stateful<gpui_kit::Div> {
    use gpui_kit::component::{Icon, h_flex};
    use gpui_kit::prelude::FluentBuilder as _;
    use gpui_kit::{InteractiveElement as _, ParentElement as _, div};
    let note = note.map(str::to_owned);
    let p = *p;
    let ink = danger.unwrap_or(p.label);
    let icon_ink = danger.unwrap_or(if current { p.accent } else { p.secondary });
    let wash = danger.map_or(p.hover, |d| d.opacity(0.12));
    let rich = note.is_some();
    h_flex()
        .id(id.into())
        .min_h(z(if rich { 46. } else { FLOAT_ITEM_H }))
        .w_full()
        .px(z(10.))
        .py(z(if rich { 6. } else { 0. }))
        .gap(z(10.))
        .rounded(z(R_FLOAT - FLOAT_PAD))
        .font_weight(FontWeight::NORMAL)
        .text_color(ink)
        .when(current, |d| d.bg(p.selection))
        .hover(move |d| d.bg(if current { p.selection } else { wash }))
        .children(icon.map(|i| div().flex_shrink_0().size(z(15.)).flex().items_center().justify_center().child(Icon::empty().path(format!("icons/{i}.svg")).size(z(15.)).text_color(icon_ink))))
        .child(
            gpui_kit::component::v_flex()
                .flex_1()
                .min_w_0()
                .gap(px(1.))
                .child(div().text_size(z(13.5)).line_height(z(18.)).truncate().child(name))
                .children(note.map(|n| div().text_size(z(12.)).line_height(z(16.)).text_color(p.tertiary).truncate().child(n))),
        )
        .children(keys.map(|k| div().pl(z(14.)).child(keycaps(&p, k))))
        .when(current, |d| d.child(div().flex_shrink_0().text_color(p.accent).child(Icon::empty().path("icons/check.svg").size(z(13.)))))
}

/// a quiet heading inside a menu, above a group of items.
pub fn float_label(p: &Palette, text: impl Into<SharedString>) -> gpui_kit::Div {
    use gpui_kit::{ParentElement as _, div};
    div().px(z(10.)).pt(z(8.)).pb(z(3.)).text_size(z(11.5)).line_height(z(16.)).font_weight(FontWeight::MEDIUM).text_color(p.tertiary).child(text.into())
}

/// the line between groups in a menu.
pub fn float_sep(p: &Palette) -> gpui_kit::Div {
    gpui_kit::div().h(px(1.)).mx(z(10.)).my(z(4.)).bg(p.separator)
}

// ---- sizes -------------------------------------------------------------------

/// the toolbar that is merged with the title bar.
pub const TOOLBAR_H: f32 = 52.0;
/// room for the window's close/minimise/zoom buttons at the leading edge.
pub const WINDOW_BUTTONS_W: f32 = 78.0;
pub const SIDEBAR_W: f32 = 264.0;
pub const SIDEBAR_MIN: f32 = 220.0;
pub const SIDEBAR_MAX: f32 = 380.0;
/// the workspace panel: wide enough to read a diff in.
pub const PANEL_W: f32 = 460.0;
pub const PANEL_MIN: f32 = 340.0;
/// text is set in a column this wide, not edge to edge.
pub const READING_W: f32 = 720.0;
/// the narrowest the transcript may get before panels give way.
pub const CONTENT_MIN: f32 = 420.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_meets_apples_contrast_floor_on_every_surface() {
        for p in [Palette::light(), Palette::dark(), Palette::orchard_light(), Palette::orchard_dark(), Palette::tern_light(), Palette::tern_dark()] {
            for (name, bg) in [("window", p.window), ("sidebar", p.sidebar)] {
                // apple: 4.5:1 for text up to 17pt.
                assert!(contrast(p.label, bg) >= 4.5, "label on {name} (dark={}): {:.2}", p.dark, contrast(p.label, bg));
                assert!(contrast(p.secondary, bg) >= 4.5, "secondary on {name} (dark={}): {:.2}", p.dark, contrast(p.secondary, bg));
            }
            assert!(contrast(p.on_accent, p.accent) >= 3.0, "button text on accent (dark={})", p.dark);
        }
    }

    #[test]
    fn the_three_meaningful_colours_stand_out_and_differ() {
        for p in [Palette::light(), Palette::dark()] {
            for (name, c) in [("blocked", p.blocked), ("failed", p.failed), ("finished", p.finished)] {
                // these mark small shapes, not text: apple's 3:1 floor for non-text.
                assert!(contrast(c, p.sidebar) >= if p.dark { 3.0 } else { 1.9 }, "{name} on sidebar (dark={}): {:.2}", p.dark, contrast(c, p.sidebar));
            }
            assert!(p.blocked != p.failed && p.failed != p.finished && p.blocked != p.finished);
        }
    }

    #[test]
    fn the_shipped_palettes_are_already_readable_so_keeping_them_readable_changes_nothing() {
        for p in [Palette::light(), Palette::dark(), Palette::orchard_light(), Palette::orchard_dark(), Palette::tern_light(), Palette::tern_dark()] {
            let mut kept = p;
            kept.keep_readable();
            assert_eq!(kept, p, "dark={}", p.dark);
        }
    }

    #[test]
    fn a_theme_that_picks_unreadable_colours_is_made_readable() {
        let mut p = Palette::light();
        p.label = ink(0xeeeeee, 1.0);
        p.secondary = ink(0xffffff, 0.2);
        p.accent = ink(0xfdfdfd, 1.0);
        p.on_accent = ink(0xffffff, 1.0);
        p.keep_readable();
        for bg in [p.window, p.sidebar, p.raised] {
            assert!(contrast(p.label, bg) >= 4.5 && contrast(p.secondary, bg) >= 4.5);
        }
        assert!(contrast(p.accent, p.window) >= 3.0 && contrast(p.on_accent, p.accent) >= 3.0);
    }

    #[test]
    fn anything_drawn_on_an_unplanned_background_stays_readable() {
        for base in [Palette::light(), Palette::dark(), Palette::orchard_light(), Palette::orchard_dark()] {
            for bg in [base.accent, base.failed, ink(0x000000, 1.0), ink(0xffff00, 1.0), ink(0x777777, 1.0), base.accent.opacity(0.3)] {
                let p = base.on(bg);
                assert!(contrast(p.label, p.window) >= 4.5, "label on {bg:?}: {:.2}", contrast(p.label, p.window));
                assert!(contrast(p.secondary, p.window) >= 4.5, "secondary on {bg:?}");
                assert!(contrast(p.on_accent, p.accent) >= 3.0);
            }
            // a planned surface changes nothing but itself.
            assert_eq!(base.on(base.window), base);
        }
    }

    #[test]
    fn light_and_dark_are_both_complete_and_opposite() {
        let (l, d) = (Palette::light(), Palette::dark());
        assert!(!l.dark && d.dark);
        assert!(luminance(l.window, l.window) > 0.9 && luminance(d.window, d.window) < 0.05);
        assert_eq!(Palette::for_appearance(WindowAppearance::VibrantDark), d);
        assert_eq!(Palette::for_appearance(WindowAppearance::Light), l);
    }

    #[test]
    fn type_styles_are_apples_published_mac_sizes() {
        assert_eq!(Type::Body.metrics().0, px(13.));
        assert_eq!(Type::Body.metrics().1, px(16.));
        assert_eq!(Type::Callout.metrics().0, px(12.));
        assert_eq!(Type::Subheadline.metrics().0, px(11.));
        assert_eq!(Type::Title.metrics().0, px(15.));
        // nothing is set smaller than apple's 10pt minimum.
        for t in [Type::Title, Type::Headline, Type::Body, Type::Callout, Type::Subheadline, Type::Prose, Type::Code] {
            assert!(t.metrics().0 >= px(10.), "{t:?}");
            assert!(t.metrics().1 > t.metrics().0, "{t:?} needs leading");
        }
    }
}
