//! images drawn under the window: the dithered backdrop behind the panes (a
//! band in the colours of the project you're in), and felt, the cloth the
//! frame is made of when the material is felt.
//!
//! it is drawn once, off the ui thread, at the room's real pixel size, and
//! handed to the gpu as one image in its atlas; after that a frame costs one
//! sprite, corners rounded by the gpu.
//! it's drawn 1:1 because the gpu samples textures smoothly, and a scaled-up
//! dither would blur its dots. a new one is drawn only when the room, the
//! project or the theme changes, and only one at a time: while the window is
//! being resized the last one stretches to fit until the next is ready.

use super::*;
use std::cell::RefCell;
use std::rc::Rc;

/// what a backdrop is drawn from. two equal keys draw the same pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Key {
    pub w: u32,
    pub h: u32,
    /// device pixels per dot, gap included.
    pub cell: u32,
    pub hue: f32,
    pub dark: bool,
    pub bg: [u8; 3],
    /// felt rather than the dithered band.
    pub felt: bool,
    /// the meadow the setup flow is drawn on (see `draw_meadow`), at frame `frame`.
    pub meadow: bool,
    pub frame: u32,
    /// frosted glass over the meadow: this image is a pane of it, at (`ox`, `oy`)
    /// in a field `fw` × `fh` (device pixels), the field seen through frosted glass.
    pub frost: bool,
    pub ox: u32,
    pub oy: u32,
    pub fw: u32,
    pub fh: u32,
}

#[derive(Default)]
pub(crate) struct State {
    texture: Option<(Key, Arc<RenderImage>)>,
    /// drawn and waiting for a frame to hand it to the gpu.
    ready: Option<(Key, Vec<u8>)>,
    drawing: Option<Key>,
}

pub(crate) type Shared = Rc<RefCell<State>>;

/// the ordered-dither threshold for a dot, from an 8×8 bayer matrix, in 0..1.
/// (the backdrop dithers by noise now: a bayer grid on dots this size reads
/// as a pattern. the setup flow's meadow wants the ordered look.)
fn bayer(x: u32, y: u32) -> f32 {
    let (mut v, mut x, mut y) = (0u32, x & 7, y & 7);
    for _ in 0..3 {
        v = (v << 2) | (((x ^ y) & 1) << 1) | (y & 1);
        x >>= 1;
        y >>= 1;
    }
    // the bits come out reversed; reverse the 6 of them.
    let r = (0..6).fold(0u32, |acc, i| acc | (((v >> i) & 1) << (5 - i)));
    (r as f32 + 0.5) / 64.0
}

/// a cheap, stable hash of a dot, in 0..1: the scattered sparks off the band.
fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let c: Hsla = gpui_kit::hsla(h.rem_euclid(1.0), s, l, 1.0);
    let c = c.to_rgb();
    [c.r, c.g, c.b]
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// the colour across the band: cool on its outer edge, pale in the middle,
/// warm and then deep on the inner. `s` runs -1..1 across it.
fn ramp(hue: f32, s: f32) -> [f32; 3] {
    let stops = [hsl(hue, 0.75, 0.42), hsl(hue + 0.04, 0.85, 0.62), hsl(hue + 0.10, 0.35, 0.82), hsl(hue + 0.52, 0.9, 0.58), hsl(hue + 0.57, 0.95, 0.42)];
    let t = ((s + 1.0) / 2.0).clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    mix(stops[i], stops[i + 1], t - i as f32)
}

/// felt: the frame's colour with a fine grain, a slight horizontal nap, and a
/// broad sheen from the upper left, the way light lies across a pile. all of
/// it a few percent either way: you feel it more than see it, and text never
/// sits on it.
fn draw_felt(key: &Key) -> Vec<u8> {
    let (w, h) = (key.w as usize, key.h as usize);
    let mut out = vec![0u8; w * h * 4];
    let base = key.bg.map(|c| c as f32 / 255.0);
    let (grain, nap, sheen) = if key.dark { (0.11, 0.07, 0.16) } else { (0.05, 0.035, 0.08) };
    let span = (w + h) as f32;
    for y in 0..h {
        let row = &mut out[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let fine = noise(x as u32, y as u32, 3) - 0.5;
            // fibres lie along the cloth: a value shared by a short run of pixels.
            let fibre = noise((x / 5) as u32, y as u32, 11) - 0.5;
            // the sheen: brightest along a broad diagonal near the upper left.
            let d = (x + y) as f32 / span - 0.22;
            let lift = (-(d * d) / 0.06).exp() * sheen - sheen * 0.35;
            let k = 1.0 + fine * grain + fibre * nap + lift;
            let c = base.map(|b| (b * k).clamp(0.0, 1.0));
            let at = x * 4;
            row[at] = (c[2] * 255.0 + 0.5) as u8;
            row[at + 1] = (c[1] * 255.0 + 0.5) as u8;
            row[at + 2] = (c[0] * 255.0 + 0.5) as u8;
            row[at + 3] = 255;
        }
    }
    out
}

/// the landing page's meadow: a glow coming up from one corner and sinking
/// into navy, like a field at dusk, ordered-dithered onto eight colours in
/// square dots with a dark grid between them. it drifts a little each frame.
fn draw_meadow(key: &Key) -> Vec<u8> {
    const PALETTE: [u32; 8] = [0x04060c, 0x0a1430, 0x0f2b4a, 0x16505a, 0x2f7d5a, 0x7eb04a, 0xc6d94e, 0xe6ec86];
    let colors = PALETTE.map(|c| [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32]);
    let (w, h, cell) = (key.w as usize, key.h as usize, key.cell.max(2) as usize);
    // the whole field this image is part of (all of it, unless it's a frosted pane).
    let (fw, fh) = if key.frost { (key.fw.max(1) as f32, key.fh.max(1) as f32) } else { (w as f32, h as f32) };
    let (fcols, frows) = ((fw / cell as f32).ceil(), (fh / cell as f32).ceil());
    let t = key.frame as f32 * 0.25;
    let hash = |x: i32, y: i32| {
        let mut h = x.wrapping_mul(374_761_393).wrapping_add(y.wrapping_mul(668_265_263));
        h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
        ((h ^ (h >> 16)) as u32) as f32 / 4_294_967_295.0
    };
    let smooth = |t: f32| t * t * (3.0 - 2.0 * t);
    let noise = |x: f32, y: f32| {
        let (xi, yi) = (x.floor() as i32, y.floor() as i32);
        let (xf, yf) = (smooth(x - x.floor()), smooth(y - y.floor()));
        let (a, b, c, d) = (hash(xi, yi), hash(xi + 1, yi), hash(xi, yi + 1), hash(xi + 1, yi + 1));
        a + (b - a) * xf + (c - a) * yf + (a - b - c + d) * xf * yf
    };
    // the field at a point (0..1 across the whole of it): the landing page's meadow.
    let field = |gx: f32, gy: f32| {
        let dx = gx - 0.82 - 0.04 * (t * 0.2).sin();
        let dy = (gy - 0.02) * 1.25;
        let blob = (-(dx * dx + dy * dy) * 2.6).exp();
        let n = noise(gx * 3.0 + t * 0.05, gy * 3.0) - 0.5;
        (0.08 + 0.6 * blob + 0.32 * (1.0 - gy) * (0.5 + gx * 0.5) + 0.18 * n - 0.15 * gy).clamp(0.0, 1.0)
    };
    let mut out = vec![0u8; w * h * 4];
    if key.frost {
        // through frosted glass the dots melt into their colours, dimmed by the
        // glass's own dark, with a fine grain so it reads as a material.
        for y in 0..h {
            for x in 0..w {
                let (gx, gy) = ((key.ox as f32 + x as f32) / fw, (key.oy as f32 + y as f32) / fh);
                let s = field(gx, gy) * 7.0;
                let i = (s as usize).min(6);
                let f = s - i as f32;
                let c = [0, 1, 2].map(|k| colors[i][k] + (colors[i + 1][k] - colors[i][k]) * f);
                let grain = (hash(x as i32, y as i32) - 0.5) * 7.0;
                let glass = [13.0, 15.0, 16.0];
                let p = [0, 1, 2].map(|k| (glass[k] * 0.62 + c[k] * 0.38 + grain).clamp(0.0, 255.0) as u8);
                let at = (y * w + x) * 4;
                out[at..at + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
            }
        }
        return out;
    }
    let (cols, rows) = (w.div_ceil(cell), h.div_ceil(cell));
    // a dot is most of its cell; the rest is the grid, darker than the dot.
    let dot = ((cell as f32) * 0.78).round().max(1.0) as usize;
    for row in 0..rows {
        for col in 0..cols {
            let v = field(col as f32 / fcols, row as f32 / frows);
            let s = v * 7.0;
            let mut i = s as usize;
            if s - i as f32 > bayer(col as u32, row as u32) {
                i += 1;
            }
            let c = colors[i.min(7)].map(|x| x as u8);
            let gap = c.map(|x| (x as f32 * 0.42) as u8);
            for py in row * cell..((row + 1) * cell).min(h) {
                for px in col * cell..((col + 1) * cell).min(w) {
                    let inside = py - row * cell < dot && px - col * cell < dot;
                    let p = if inside { c } else { gap };
                    let at = (py * w + px) * 4;
                    out[at..at + 4].copy_from_slice(&[p[2], p[1], p[0], 255]);
                }
            }
        }
    }
    out
}

/// an image's pixels, bgra, `key.w` × `key.h`.
pub(crate) fn draw(key: &Key) -> Vec<u8> {
    if key.meadow {
        return draw_meadow(key);
    }
    if key.felt {
        return draw_felt(key);
    }
    let (w, h, cell) = (key.w as usize, key.h as usize, key.cell.max(2) as usize);
    let mut out = vec![0u8; w * h * 4];
    let bg = key.bg.map(|c| c as f32 / 255.0);
    // between the dots: a touch darker than the room, so the grid is felt more than seen.
    let gap = bg.map(|c| c * if key.dark { 0.86 } else { 0.96 });
    let to8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let gap_px = [to8(gap[2]), to8(gap[1]), to8(gap[0]), 255];
    let cols = w.div_ceil(cell);
    let rows = h.div_ceil(cell);
    // the band: an arc of a big circle whose centre is off to the lower right.
    let scale = w.max(h) as f32;
    // it crosses the upper left, clear of the middle where the cards sit.
    let (cxp, cyp, radius, width) = (1.25, 1.35, 1.55, 0.075);
    let levels = 4.0;
    let mut dot_row = vec![0u8; w * 4];
    let mut gap_row = vec![0u8; w * 4];
    for px in gap_row.chunks_exact_mut(4) {
        px.copy_from_slice(&gap_px);
    }
    for row in 0..rows {
        for col in 0..cols {
            let x = (col * cell) as f32 / scale;
            let y = (row * cell) as f32 / scale;
            let s = (((x - cxp).powi(2) + (y - cyp).powi(2)).sqrt() - radius) / width;
            // bright in the band, a short soft falloff, then the dark.
            let core = (-s * s).exp();
            let fall = (-(s * 0.45).powi(2)).exp() * 0.22;
            // the dark isn't empty: a few sparks, in the band's colours.
            let n = noise(col as u32, row as u32, 7);
            let spark = if n > 0.992 { 0.55 } else { 0.0 };
            let amount = (core + fall).max(spark).min(1.0);
            // how much colour a dot gets, dithered to a few steps by noise, so
            // the room's own colour is never touched where the band isn't.
            let t = noise(col as u32, row as u32, 1931);
            let step = ((amount * (levels - 1.0) + t - 0.5).round() / (levels - 1.0)).clamp(0.0, 1.0);
            let hue_at = if spark > 0.0 && core < 0.05 { (n * 997.0).fract() * 2.0 - 1.0 } else { s.clamp(-1.0, 1.0) };
            let c = mix(bg, ramp(key.hue, hue_at), step);
            let px = [to8(c[2]), to8(c[1]), to8(c[0]), 255];
            // the dot, then a one-pixel gap to its right.
            for dx in 0..cell {
                let xi = col * cell + dx;
                if xi >= w {
                    break;
                }
                let at = xi * 4;
                dot_row[at..at + 4].copy_from_slice(if dx == cell - 1 { &gap_px } else { &px });
            }
        }
        // the dot's rows, then a one-pixel gap below it.
        for dy in 0..cell {
            let yi = row * cell + dy;
            if yi >= h {
                break;
            }
            let src = if dy == cell - 1 { &gap_row } else { &dot_row };
            out[yi * w * 4..(yi + 1) * w * 4].copy_from_slice(src);
        }
    }
    out
}

/// the hue a project's backdrop is drawn in: its tile's.
pub(crate) fn project_hue(name: &str) -> f32 {
    super::sidebar::project_color(name).h
}

impl Shell {
    /// what's behind the panes: nothing, unless the theme asks.
    pub(crate) fn backdrop_kind(&self) -> crate::ui::Backdrop {
        self.ui.theme.backdrop.unwrap_or_default()
    }

    /// whether there's anything behind the panes.
    pub(crate) fn backdrop_on(&self) -> bool {
        self.backdrop_kind() != crate::ui::Backdrop::None
    }

    /// the hue for the room: the open session's project, else the one a new session would start in.
    fn backdrop_hue(&self) -> f32 {
        let name = match self.model.open_summary().filter(|_| !self.show_new) {
            Some(s) => project_name(s),
            None => self.new_folder().as_deref().map(projects::folder_name).unwrap_or_default(),
        };
        if name.is_empty() { self.palette.get().accent.h } else { project_hue(&name) }
    }

    /// the backdrop as an element filling its parent.
    pub(crate) fn backdrop_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let (hue, dark) = (self.backdrop_hue(), p.dark);
        let bg = rgb8(p.window);
        // a dot of about two points, whatever the screen, drawn 1:1 so its edges stay sharp.
        image_el(self.backdrop.clone(), cx.entity().downgrade(), theme::R_PANEL, move |w, h, scale| {
            Some(Key { w: (w * scale).round() as u32, h: (h * scale).round() as u32, cell: (2.0 * scale).round().max(2.0) as u32, hue, dark, bg, felt: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 })
        })
    }

    /// whether the frame is felt.
    pub(crate) fn felt_on(&self) -> bool {
        self.ui.theme.felt.unwrap_or(false) && !self.ui.theme.glass.unwrap_or(false)
    }

    /// the felt under the whole window.
    pub(crate) fn felt_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        // the frame was cleared to let the felt through; its colour is still in it.
        let bg = rgb8(p.sidebar.opacity(1.0));
        let dark = p.dark;
        // a point per pixel: the gpu's smoothing softens the grain, as cloth is soft.
        image_el(self.felt.clone(), cx.entity().downgrade(), 0.0, move |w, h, _| Some(Key { w: w.round() as u32, h: h.round() as u32, cell: 1, hue: 0.0, dark, bg, felt: true, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 }))
    }
}

fn rgb8(c: Hsla) -> [u8; 3] {
    let c = c.to_rgb();
    [(c.r * 255.0).round() as u8, (c.g * 255.0).round() as u8, (c.b * 255.0).round() as u8]
}

/// an image drawn off the ui thread for the size it's shown at, handed to the
/// gpu once, and painted every frame as one sprite. `key_for` names the image
/// for a size in points and a scale; a different key draws a new one.
pub(crate) fn image_el(state: Shared, me: WeakEntity<Shell>, radius: f32, key_for: impl Fn(f32, f32, f32) -> Option<Key> + 'static) -> AnyElement {
    image_el_corners(state, me, Corners::all(px(radius)), key_for)
}

/// the same, its corners rounded each its own way.
pub(crate) fn image_el_corners(state: Shared, me: WeakEntity<Shell>, corners: Corners<Pixels>, key_for: impl Fn(f32, f32, f32) -> Option<Key> + 'static) -> AnyElement {
    image_el_at(state, me, corners, move |b, scale| key_for(f32::from(b.size.width), f32::from(b.size.height), scale))
}

/// the same, its key made knowing where on the window it's painted (a frosted
/// pane needs to know which part of the field is behind it).
pub(crate) fn image_el_at(state: Shared, me: WeakEntity<Shell>, corners: Corners<Pixels>, key_for: impl Fn(Bounds<Pixels>, f32) -> Option<Key> + 'static) -> AnyElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, cx| {
            let Some(key) = key_for(bounds, window.scale_factor()) else { return };
            if key.w == 0 || key.h == 0 {
                return;
            }
            let mut st = state.borrow_mut();
            // a freshly drawn one goes to the gpu, and the old one leaves its atlas.
            if let Some((k, bytes)) = st.ready.take()
                && let Some(buf) = image::RgbaImage::from_raw(k.w, k.h, bytes)
            {
                let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
                if let Some((_, old)) = st.texture.replace((k, fresh)) {
                    let _ = window.drop_image(old);
                }
            }
            if st.texture.as_ref().map(|(k, _)| *k) != Some(key) && st.drawing.is_none() {
                st.drawing = Some(key);
                let (state, me) = (state.clone(), me.clone());
                let work = cx.background_executor().spawn(async move { draw(&key) });
                cx.spawn(async move |cx| {
                    let bytes = work.await;
                    {
                        let mut st = state.borrow_mut();
                        st.drawing = None;
                        st.ready = Some((key, bytes));
                    }
                    let _ = me.update(cx, |_, cx| cx.notify());
                })
                .detach();
            }
            if let Some((_, t)) = st.texture.clone() {
                let _ = window.paint_image(bounds, bounds, corners, t, 0, false);
            }
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Key, bayer, draw};

    #[test]
    fn the_bayer_thresholds_are_each_used_once() {
        let mut seen: Vec<u32> = (0..8).flat_map(|y| (0..8).map(move |x| (bayer(x, y) * 64.0) as u32)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..64).collect::<Vec<_>>());
    }

    /// speed is checked where the code is built as it ships; CI builds it
    /// unoptimized, to compile quickly, and there only correctness is.
    fn timed() -> bool {
        std::env::var_os("CI").is_none()
    }

    /// the best of three draws: a busy machine slows one, a slow drawing slows all.
    fn fastest(key: &Key) -> (Vec<u8>, std::time::Duration) {
        let mut best = None;
        let mut px = Vec::new();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            px = draw(key);
            let took = started.elapsed();
            best = Some(best.map_or(took, |b: std::time::Duration| b.min(took)));
        }
        (px, best.unwrap_or_default())
    }

    #[test]
    fn a_backdrop_is_dots_with_gaps_and_cheap_to_draw() {
        let key = Key { w: 2880, h: 1800, cell: 4, hue: 0.6, dark: true, bg: [30, 30, 30], felt: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 };
        let (px, took) = fastest(&key);
        assert_eq!(px.len(), 2880 * 1800 * 4);
        // the last column and row of every dot are the gap, all one colour.
        let at = |x: usize, y: usize| &px[(y * 2880 + x) * 4..(y * 2880 + x) * 4 + 4];
        assert_eq!(at(3, 0), at(7, 5));
        assert_eq!(at(0, 3), at(3, 3));
        // somewhere the band is lit: not every dot is the room's colour.
        assert!(px.chunks_exact(4).any(|p| p[0] > 120 || p[1] > 120 || p[2] > 120), "the band shows");
        // a retina window, off the ui thread: well under a frame's worth of a second.
        assert!(!timed() || took.as_millis() < 250, "drawing it took {took:?}");
    }

    #[test]
    fn felt_stays_close_to_its_colour_and_is_cheap_to_draw() {
        let key = Key { w: 1440, h: 900, cell: 1, hue: 0.0, dark: true, bg: [58, 18, 24], felt: true, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 };
        let (px, took) = fastest(&key);
        assert_eq!(px.len(), 1440 * 900 * 4);
        // grain, not pattern: every pixel within a few steps of the cloth's colour.
        let red = px.chunks_exact(4).map(|p| i32::from(p[2]));
        let (lo, hi) = red.fold((255, 0), |(lo, hi), r| (lo.min(r), hi.max(r)));
        assert!(lo >= 58 - 16 && hi <= 58 + 16, "the red channel stays near 58: {lo}..{hi}");
        assert!(hi > lo, "and it isn't flat");
        assert!(!timed() || took.as_millis() < 250, "drawing it took {took:?}");
    }
}
