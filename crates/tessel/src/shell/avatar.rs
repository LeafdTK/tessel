//! the little dithered squares that stand for a machine or a project: a
//! glow of two colours picked from the name, ordered-dithered in big pixels,
//! so each one is recognisable at a glance and the same every time.
//!
//! drawn once per name and size (in device pixels, so the dots stay crisp),
//! kept, and painted as one sprite.

use super::*;
use std::cell::RefCell;

/// three stops each: deep, glow, and the hot centre of the glow. identity
/// only: none of them is a status colour.
const SETS: [[[u8; 3]; 3]; 8] = [
    [[0x10, 0x2a, 0x12], [0x6f, 0xb8, 0x3c], [0xe6, 0xf5, 0x8a]], // moss, leaf, lime
    [[0x1a, 0x0e, 0x3a], [0xd8, 0x4a, 0x3a], [0xff, 0xc0, 0x6a]], // violet, ember, peach
    [[0x04, 0x0d, 0x33], [0x1f, 0x6f, 0xe0], [0x9f, 0xe4, 0xff]], // navy, cobalt, sky
    [[0x26, 0x08, 0x2a], [0xc0, 0x3a, 0x8f], [0xff, 0xb0, 0xd8]], // plum, magenta, blush
    [[0x03, 0x22, 0x24], [0x16, 0xa0, 0x8c], [0xa8, 0xff, 0xd8]], // deep teal, jade, mint
    [[0x2a, 0x12, 0x04], [0xe0, 0x8a, 0x10], [0xff, 0xe6, 0x7a]], // umber, amber, butter
    [[0x10, 0x12, 0x18], [0x5c, 0x6a, 0x82], [0xdc, 0xe4, 0xf0]], // ink, slate, frost
    [[0x0a, 0x10, 0x2a], [0x6a, 0x4c, 0xe8], [0x5f, 0xf0, 0xe0]], // night, iris, cyan
];

thread_local! {
    static DRAWN: RefCell<HashMap<(String, u32), Arc<RenderImage>>> = RefCell::new(HashMap::new());
}

fn hash(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
}

/// an 4×4 ordered-dither threshold, 0 up to just under 1.
fn bayer4(x: u32, y: u32) -> f32 {
    const M: [[u8; 4]; 4] = [[0, 8, 2, 10], [12, 4, 14, 6], [3, 11, 1, 9], [15, 7, 13, 5]];
    (f32::from(M[(y & 3) as usize][(x & 3) as usize]) + 0.5) / 16.0
}

/// the picture for `seed`, `n` device pixels square, bgra: a soft glow on
/// a deep ground, with a fine ordered dither for grain.
pub(crate) fn draw(seed: &str, n: u32) -> Vec<u8> {
    let h = hash(seed);
    let set = SETS[(h % SETS.len() as u64) as usize];
    // where the glow sits, how far it reaches, and which way the ground leans.
    let gx = 0.2 + ((h >> 8) % 60) as f32 / 100.0;
    let gy = 0.15 + ((h >> 16) % 50) as f32 / 100.0;
    let reach = 0.55 + ((h >> 24) % 30) as f32 / 100.0;
    let lean = (h >> 32) % 4;
    // grain about two device pixels across, however big the picture.
    let cell = (n / 24).max(1);
    let lerp = |a: [u8; 3], b: [u8; 3], t: f32| [0, 1, 2].map(|i| f32::from(a[i]) + (f32::from(b[i]) - f32::from(a[i])) * t);
    let mut out = Vec::with_capacity((n * n * 4) as usize);
    for y in 0..n {
        for x in 0..n {
            let (cx, cy) = (x / cell, y / cell);
            let (u, v) = ((x as f32 + 0.5) / n as f32, (y as f32 + 0.5) / n as f32);
            let d = ((u - gx).powi(2) + (v - gy).powi(2)).sqrt() / reach;
            let glow = (1.0 - d).clamp(0.0, 1.0).powf(1.4);
            let ground = match lean {
                0 => (u + v) / 2.0,
                1 => (1.0 - u + v) / 2.0,
                2 => v,
                _ => 1.0 - v,
            } * 0.35;
            let t = (glow + ground).clamp(0.0, 1.0);
            // deep to glow to hot, in eight steps, dithered between neighbours.
            let steps = 8.0;
            let q = t * steps;
            let level = (q.floor() + if q.fract() > bayer4(cx, cy) { 1.0 } else { 0.0 }).min(steps) / steps;
            let c = if level < 0.5 { lerp(set[0], set[1], level * 2.0) } else { lerp(set[1], set[2], level * 2.0 - 1.0) };
            out.extend_from_slice(&[c[2].round() as u8, c[1].round() as u8, c[0].round() as u8, 255]);
        }
    }
    out
}

fn image(seed: &str, n: u32) -> Option<Arc<RenderImage>> {
    DRAWN.with(|d| {
        let mut d = d.borrow_mut();
        if let Some(i) = d.get(&(seed.to_owned(), n)) {
            return Some(i.clone());
        }
        let buf = image::RgbaImage::from_raw(n, n, draw(seed, n))?;
        let fresh = Arc::new(RenderImage::new(vec![image::Frame::new(buf)]));
        d.insert((seed.to_owned(), n), fresh.clone());
        Some(fresh)
    })
}

/// `seed`'s square, `size` points, its corners rounded by `radius`.
pub(crate) fn avatar(seed: &str, size: f32, radius: f32) -> impl IntoElement {
    let seed = seed.to_owned();
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let n = (f32::from(bounds.size.width) * window.scale_factor()).round().max(1.0) as u32;
            if let Some(img) = image(&seed, n) {
                let _ = window.paint_image(bounds, bounds, Corners::all(px(radius)), img, 0, false);
            }
        },
    )
    .size(px(size))
    .flex_shrink_0()
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_is_always_the_same_picture_and_names_differ() {
        let a = super::draw("orchard", 40);
        assert_eq!(a.len(), 40 * 40 * 4);
        assert_eq!(a, super::draw("orchard", 40));
        assert_ne!(a, super::draw("tessel", 40));
    }
}
