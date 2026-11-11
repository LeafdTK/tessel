//! mermaid diagrams and tex math as self-contained svg.
//!
//! every `<text>` is already converted to paths in the returned document, so
//! it draws the same wherever it is rasterised and needs no fonts there.

mod fonts;
mod math;
mod mermaid;

use std::any::Any;

/// a finished drawing. `width` and `height` are its natural size in px.
#[derive(Debug, Clone, PartialEq)]
pub struct Svg {
    pub data: String,
    pub width: f32,
    pub height: f32,
}

/// the colours and text size of the surface the drawing is placed on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Style {
    pub dark: bool,
    pub text: [u8; 3],
    pub muted: [u8; 3],
    pub background: [u8; 3],
    pub accent: [u8; 3],
    pub font_px: f32,
}

/// renders the body of a ```` ```mermaid ```` block.
pub fn mermaid(source: &str, style: &Style) -> Result<Svg, String> {
    isolated(|| mermaid::render(source, style))
}

/// renders `$..$` (inline) or `$$..$$` (display) tex math.
pub fn math(tex: &str, display: bool, style: &Style) -> Result<Svg, String> {
    isolated(|| math::render(tex, display, style))
}

// layout recurses with the nesting of the input, and callers may sit on a
// worker thread with a small stack. a stack overflow aborts the whole process
// and cannot be caught, so the work gets a stack of known size instead.
const RENDER_STACK_BYTES: usize = 64 * 1024 * 1024;

fn isolated<F>(render: F) -> Result<Svg, String>
where
    F: FnOnce() -> Result<Svg, String> + Send,
{
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .name("tessel-rich".into())
            .stack_size(RENDER_STACK_BYTES)
            .spawn_scoped(scope, render)
            .map_err(|error| format!("could not start the renderer: {error}"))?
            .join()
            .unwrap_or_else(|panic| Err(format!("renderer failed: {}", panic_text(&*panic))))
    })
}

fn panic_text(panic: &(dyn Any + Send)) -> &str {
    panic
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("unknown error")
}

pub(crate) fn hex(colour: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", colour[0], colour[1], colour[2])
}

/// `from` moved `amount` (0..=1) of the way towards `to`.
pub(crate) fn mix(from: [u8; 3], to: [u8; 3], amount: f32) -> [u8; 3] {
    std::array::from_fn(|i| {
        let (a, b) = (f32::from(from[i]), f32::from(to[i]));
        (a + (b - a) * amount).round().clamp(0.0, 255.0) as u8
    })
}

/// parses a drawing, turns its text into paths and writes it back out.
pub(crate) fn finish(svg: &str) -> Result<Svg, String> {
    let tree = usvg::Tree::from_str(svg, fonts::FONTS.options())
        .map_err(|error| format!("could not read the rendered drawing: {error}"))?;
    let size = tree.size();
    Ok(Svg {
        data: tree.to_string(&usvg::WriteOptions::default()),
        width: size.width(),
        height: size.height(),
    })
}
