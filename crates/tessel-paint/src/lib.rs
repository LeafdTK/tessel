#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Key {
    pub w: u32,
    pub h: u32,
    pub cell: u32,
    pub hue: f32,
    pub dark: bool,
    pub bg: [u8; 3],
    pub felt: bool,
    pub field: u8,
    pub fade: bool,
    pub meadow: bool,
    pub frame: u32,
    pub frost: bool,
    pub ox: u32,
    pub oy: u32,
    pub fw: u32,
    pub fh: u32,
}

pub fn bayer(x: u32, y: u32) -> f32 {
    let (mut v, mut x, mut y) = (0u32, x & 7, y & 7);
    for _ in 0..3 {
        v = (v << 2) | (((x ^ y) & 1) << 1) | (y & 1);
        x >>= 1;
        y >>= 1;
    }
    let r = (0..6).fold(0u32, |acc, i| acc | (((v >> i) & 1) << (5 - i)));
    (r as f32 + 0.5) / 64.0
}

fn noise(x: u32, y: u32, seed: u32) -> f32 {
    let mut h = x.wrapping_mul(0x9E37_79B1) ^ y.wrapping_mul(0x85EB_CA77) ^ seed.wrapping_mul(0xC2B2_AE3D);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    (h & 0xFFFF) as f32 / 65535.0
}

fn hsl(h: f32, s: f32, l: f32) -> [f32; 3] {
    let h = h.rem_euclid(1.0) * 6.0;
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let x = c * (1.0 - (h % 2.0 - 1.0).abs());
    let m = l - c / 2.0;
    let (r, g, b) = match h as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + m, g + m, b + m]
}

#[cfg(test)]
mod tests {
    #[test]
    fn hsl_matches_the_usual_conversion() {
        let near = |a: [f32; 3], b: [f32; 3]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-4);
        assert!(near(super::hsl(0.0, 1.0, 0.5), [1.0, 0.0, 0.0]));
        assert!(near(super::hsl(1.0 / 3.0, 1.0, 0.5), [0.0, 1.0, 0.0]));
        assert!(near(super::hsl(2.0 / 3.0, 1.0, 0.25), [0.0, 0.0, 0.5]));
        assert!(near(super::hsl(0.5, 0.0, 0.7), [0.7, 0.7, 0.7]));
    }
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

fn ramp(hue: f32, s: f32) -> [f32; 3] {
    let stops = [hsl(hue, 0.75, 0.42), hsl(hue + 0.04, 0.85, 0.62), hsl(hue + 0.10, 0.35, 0.82), hsl(hue + 0.52, 0.9, 0.58), hsl(hue + 0.57, 0.95, 0.42)];
    let t = ((s + 1.0) / 2.0).clamp(0.0, 1.0) * (stops.len() - 1) as f32;
    let i = (t.floor() as usize).min(stops.len() - 2);
    mix(stops[i], stops[i + 1], t - i as f32)
}

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
            let fibre = noise((x / 5) as u32, y as u32, 11) - 0.5;
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

fn draw_meadow(key: &Key) -> Vec<u8> {
    const PALETTE: [u32; 8] = [0x04060c, 0x0a1430, 0x0f2b4a, 0x16505a, 0x2f7d5a, 0x7eb04a, 0xc6d94e, 0xe6ec86];
    let colors = PALETTE.map(|c| [((c >> 16) & 255) as f32, ((c >> 8) & 255) as f32, (c & 255) as f32]);
    let (w, h, cell) = (key.w as usize, key.h as usize, key.cell.max(2) as usize);
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
    let field = |gx: f32, gy: f32| {
        let dx = gx - 0.82 - 0.04 * (t * 0.2).sin();
        let dy = (gy - 0.02) * 1.25;
        let blob = (-(dx * dx + dy * dy) * 2.6).exp();
        let n = noise(gx * 3.0 + t * 0.05, gy * 3.0) - 0.5;
        (0.08 + 0.6 * blob + 0.32 * (1.0 - gy) * (0.5 + gx * 0.5) + 0.18 * n - 0.15 * gy).clamp(0.0, 1.0)
    };
    let mut out = vec![0u8; w * h * 4];
    if key.frost {
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

const FIELD_DARK: [u32; 7] = [0x0b1d22, 0x16393d, 0x2c6262, 0x4f8d86, 0x86b9a2, 0xc4d8ad, 0xece4c4];
const FIELD_LIGHT: [u32; 6] = [0x2c4a43, 0x4f7c6c, 0x7fa88c, 0xa9c7a2, 0xcfdcb4, 0xe8e6c6];
const BAYER: [[u8; 8]; 8] = [
    [0, 32, 8, 40, 2, 34, 10, 42],
    [48, 16, 56, 24, 50, 18, 58, 26],
    [12, 44, 4, 36, 14, 46, 6, 38],
    [60, 28, 52, 20, 62, 30, 54, 22],
    [3, 35, 11, 43, 1, 33, 9, 41],
    [51, 19, 59, 27, 49, 17, 57, 25],
    [15, 47, 7, 39, 13, 45, 5, 37],
    [63, 31, 55, 23, 61, 29, 53, 21],
];

fn smooth_noise(x: f32, y: f32, seed: u32) -> f32 {
    let (xi, yi) = (x.floor(), y.floor());
    let (xf, yf) = (x - xi, y - yi);
    let (sx, sy) = (xf * xf * (3.0 - 2.0 * xf), yf * yf * (3.0 - 2.0 * yf));
    let at = |dx: f32, dy: f32| noise((xi + dx) as i64 as u32, (yi + dy) as i64 as u32, seed);
    let (a, b, c, d) = (at(0.0, 0.0), at(1.0, 0.0), at(0.0, 1.0), at(1.0, 1.0));
    a + (b - a) * sx + (c - a) * sy + (a - b - c + d) * sx * sy
}

pub fn field_value(col: usize, row: usize, light: bool) -> f32 {
    let (x, y) = (col as f32, row as f32);
    let warp = smooth_noise(x / 31.0, y / 31.0, 11) * 6.0;
    let wave = ((x * 0.11 + y * 0.07 + warp).sin() * 0.5 + 0.5).powf(2.2);
    let blob = smooth_noise(x / 17.0 + 3.0, y / 17.0, 29);
    let v = 0.55 * wave + 0.45 * blob;
    if light { 0.35 + 0.65 * v } else { 0.08 + 0.92 * v }
}

fn draw_field(key: &Key) -> Vec<u8> {
    let (w, h, cell) = (key.w as usize, key.h as usize, key.cell.max(3) as usize);
    let light = key.field == 2;
    let ramp: Vec<[u8; 3]> = if light { FIELD_LIGHT.to_vec() } else { FIELD_DARK.to_vec() }.into_iter().map(|c| [(c >> 16) as u8, (c >> 8 & 0xff) as u8, (c & 0xff) as u8]).collect();
    let n = ramp.len() as f32;
    let gap = if light { [0xdb, 0xdb, 0xc4] } else { [0x07, 0x10, 0x12] };
    let mut out = vec![0u8; w * h * 4];
    for y in 0..h {
        let row = y / cell;
        for x in 0..w {
            let col = x / cell;
            let on_gap = x % cell == 0 || y % cell == 0;
            let c = if on_gap {
                gap
            } else {
                let v = field_value(col, row, light).clamp(0.0, 1.0);
                let t = BAYER[row % 8][col % 8] as f32 / 64.0;
                let i = ((v * (n - 1.0)) + t - 0.5).round().clamp(0.0, n - 1.0) as usize;
                ramp[i]
            };
            let at = (y * w + x) * 4;
            if key.fade {
                let t = ((x as f32 / w as f32 - 0.4) / 0.6).clamp(0.0, 1.0);
                let edge = ((w - x) as f32 / (cell as f32 * 6.0)).clamp(0.0, 1.0);
                let a = if on_gap { 0 } else { (t * t * edge * if light { 170.0 } else { 120.0 }) as u8 };
                out[at..at + 4].copy_from_slice(&[c[2], c[1], c[0], a]);
            } else {
                out[at..at + 4].copy_from_slice(&[c[2], c[1], c[0], 255]);
            }
        }
    }
    out
}

pub fn draw(key: &Key) -> Vec<u8> {
    if key.field > 0 {
        return draw_field(key);
    }
    if key.meadow {
        return draw_meadow(key);
    }
    if key.felt {
        return draw_felt(key);
    }
    let (w, h, cell) = (key.w as usize, key.h as usize, key.cell.max(2) as usize);
    let mut out = vec![0u8; w * h * 4];
    let bg = key.bg.map(|c| c as f32 / 255.0);
    let gap = bg.map(|c| c * if key.dark { 0.86 } else { 0.96 });
    let to8 = |c: f32| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    let gap_px = [to8(gap[2]), to8(gap[1]), to8(gap[0]), 255];
    let cols = w.div_ceil(cell);
    let rows = h.div_ceil(cell);
    let scale = w.max(h) as f32;
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
            let core = (-s * s).exp();
            let fall = (-(s * 0.45).powi(2)).exp() * 0.22;
            let n = noise(col as u32, row as u32, 7);
            let spark = if n > 0.992 { 0.55 } else { 0.0 };
            let amount = (core + fall).max(spark).min(1.0);
            let t = noise(col as u32, row as u32, 1931);
            let step = ((amount * (levels - 1.0) + t - 0.5).round() / (levels - 1.0)).clamp(0.0, 1.0);
            let hue_at = if spark > 0.0 && core < 0.05 { (n * 997.0).fract() * 2.0 - 1.0 } else { s.clamp(-1.0, 1.0) };
            let c = mix(bg, ramp(key.hue, hue_at), step);
            let px = [to8(c[2]), to8(c[1]), to8(c[0]), 255];
            for dx in 0..cell {
                let xi = col * cell + dx;
                if xi >= w {
                    break;
                }
                let at = xi * 4;
                dot_row[at..at + 4].copy_from_slice(if dx == cell - 1 { &gap_px } else { &px });
            }
        }
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
