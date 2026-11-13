//! the little tiles that stand for a project or a machine: a diagonal
//! gradient of two colours picked from the name (the same every time), with
//! the project's first letter or the machine's kind on it in white. small
//! and flat on purpose: they have to read at 16 points.

use super::*;

/// light to deep, in pairs that read as one colour each. identity only:
/// none of them is a status colour, and white reads on all of them.
const PAIRS: [(u32, u32); 8] = [
    (0x6fcf8a, 0x24875a), // green
    (0xff9f6b, 0xd9443a), // coral
    (0x6cb2ff, 0x3a59d6), // blue
    (0xf28fcb, 0xa8418f), // pink
    (0x56d6c2, 0x1d7f86), // teal
    (0xffc95c, 0xd07a12), // amber
    (0xae9dff, 0x6447dc), // violet
    (0x9db0c6, 0x4b5d74), // slate
];

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// `seed`'s two colours.
pub(crate) fn colors(seed: &str) -> (Hsla, Hsla) {
    let (a, b) = PAIRS[(hash(seed) % PAIRS.len() as u64) as usize];
    (gpui_kit::rgb(a).into(), gpui_kit::rgb(b).into())
}

/// a tile in `seed`'s colours, `size` points, its corners rounded by `radius`.
pub(crate) fn tile(seed: &str, size: f32, radius: f32) -> Div {
    let (light, deep) = colors(seed);
    div()
        .size(px(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(radius))
        .bg(linear_gradient(135., linear_color_stop(light, 0.), linear_color_stop(deep, 1.)))
        // a hairline inside, so a pale tile still has an edge on a pale sidebar.
        .border_1()
        .border_color(gpui_kit::black().opacity(0.08))
}

/// a project's tile: its first letter on its colours.
pub(crate) fn letter(name: &str, size: f32, radius: f32) -> Div {
    let l = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "·".into());
    tile(name, size, radius).text_size(px(size * 0.58)).font_weight(FontWeight::BOLD).text_color(gpui_kit::white()).child(l)
}

/// a machine's tile: what it is (`laptop`, `server`, `cloud`) on its colours.
pub(crate) fn machine(seed: &str, icon: &str, size: f32, radius: f32) -> Div {
    tile(seed, size, radius).child(gpui_kit::component::Icon::empty().path(format!("icons/{icon}.svg")).size(px(size * 0.55)).text_color(gpui_kit::white()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_always_gets_the_same_colours() {
        assert_eq!(super::colors("orchard"), super::colors("orchard"));
    }
}
