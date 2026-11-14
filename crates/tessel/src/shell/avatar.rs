//! the little tiles that stand for a project or a machine: plain on purpose.
//! a quiet square in the window's own fill, with the project's first letter or
//! the machine's kind on it. no colours of their own: they have to read at 16
//! points and never look generated.

use super::*;

/// a plain tile, `size` points, its corners rounded by `radius`.
pub(crate) fn tile(size: f32, radius: f32, p: &Palette) -> Div {
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius))
        .bg(p.fill)
        .border_1()
        .border_color(p.separator)
        .text_color(p.secondary)
}

/// a project's tile: its first letter.
pub(crate) fn letter(name: &str, size: f32, radius: f32, p: &Palette) -> Div {
    let l = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "·".into());
    tile(size, radius, p).text_size(px(size * 0.55)).font_weight(FontWeight::SEMIBOLD).child(l)
}

/// a machine's tile: what it is (`laptop`, `server`, `cloud`).
pub(crate) fn machine(icon: &str, size: f32, radius: f32, p: &Palette) -> Div {
    tile(size, radius, p).child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(size * 0.55)).text_color(p.secondary))
}
