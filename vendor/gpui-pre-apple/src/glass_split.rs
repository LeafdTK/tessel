//! tessel's patch: real liquid glass over gpui's own drawing.
//!
//! apple's glass (`NSGlassEffectView`) bends whatever is composited under it,
//! but gpui draws a window into one metal layer, so glass can only sit under
//! all of it or over all of it. this splits the frame into layers with the
//! app's panes of glass between them: layer 0 is the window's own, and each
//! pane of glass `i` has layer `i + 1` just over it.
//!
//! a pane of glass is told by a mark: a quad whose border colour has `MARK`'s
//! hue and saturation, clear, its lightness saying which pane it is (`mark`).
//! everything drawn after a mark and over its quad goes above that glass; the
//! rest stays under it, where the glass bends it.

use gpui::{Bounds, DrawOrder, Hsla, ScaledPixels, Scene};
use std::sync::Mutex;

/// the border colour of the mark for pane of glass 0 (see `mark`).
pub const MARK: Hsla = Hsla { h: 0.618_034, s: 0.271_828, l: 0.0, a: 0.0 };

/// at most this many panes of glass in a window.
pub const MAX: usize = 63;

/// at most this many depths of glass, each a layer of its own.
pub const LEVELS: usize = 4;

/// the border colour of the mark for pane of glass `i`, counting in the order
/// they're painted (later ones are over earlier ones where they overlap).
pub fn mark(i: usize) -> Hsla {
    Hsla { l: (i.min(MAX) as f32 + 0.5) / 64.0, ..MARK }
}

fn which(c: &Hsla) -> Option<usize> {
    (c.a == 0.0 && c.h == MARK.h && c.s == MARK.s && c.l > 0.0).then(|| (c.l * 64.0) as usize)
}

/// (the window's layer, the layers over its panes of glass in order), as
/// pointers: the app owns them (its views retain them) and takes them away
/// here before freeing them.
static TOPS: Mutex<Vec<(usize, Vec<usize>)>> = Mutex::new(Vec::new());

/// draw what's over the window's panes of glass (the window whose metal layer
/// is `main`) into these `CAMetalLayer`s, the first over glass 0 and so on.
pub fn set_top_layers(main: usize, tops: Vec<usize>) {
    let mut all = TOPS.lock().unwrap_or_else(|e| e.into_inner());
    all.retain(|(m, _)| *m != main);
    if !tops.is_empty() {
        all.push((main, tops));
    }
}

pub(crate) fn top_layers(main: usize) -> Vec<usize> {
    let all = TOPS.lock().unwrap_or_else(|e| e.into_inner());
    all.iter().find(|(m, _)| *m == main).map(|(_, t)| t.clone()).unwrap_or_default()
}

/// which layer each primitive of a frame is drawn into, kind by kind, in the
/// scene's own order.
#[derive(Default)]
pub(crate) struct Layers {
    pub shadows: Vec<u8>,
    pub quads: Vec<u8>,
    pub paths: Vec<u8>,
    pub underlines: Vec<u8>,
    pub monochrome_sprites: Vec<u8>,
    pub polychrome_sprites: Vec<u8>,
    pub surfaces: Vec<u8>,
    /// the highest layer anything is in.
    pub top: usize,
}

/// the frame's marks, as (pane of glass, its order, where it is), in the
/// order they were painted.
pub(crate) fn marks(scene: &Scene) -> Vec<(usize, DrawOrder, Bounds<ScaledPixels>)> {
    let mut marks: Vec<_> = scene
        .quads
        .iter()
        .filter_map(|q| Some((which(&q.border_color)?, q.order, q.bounds.intersect(&q.content_mask.bounds))))
        .collect();
    marks.sort_by_key(|m| m.0);
    marks
}

/// how deep each pane of glass sits: 0, unless it's over one painted before
/// it, then one more than the deepest of those. panes at one depth share a
/// layer over them: a column of chat bubbles is one layer, not one each.
/// rectangles are [x, y, w, h]; the app works it out the same way.
pub fn depths(rects: &[[f64; 4]]) -> Vec<usize> {
    let overlap = |a: &[f64; 4], b: &[f64; 4]| {
        let w = (a[0] + a[2]).min(b[0] + b[2]) - a[0].max(b[0]);
        let h = (a[1] + a[3]).min(b[1] + b[3]) - a[1].max(b[1]);
        w > 1.0 && h > 1.0
    };
    let mut out: Vec<usize> = Vec::with_capacity(rects.len());
    for (i, r) in rects.iter().enumerate() {
        let d = (0..i).filter(|&j| overlap(&rects[j], r)).map(|j| out[j] + 1).max().unwrap_or(0);
        out.push(d);
    }
    out
}

/// where each primitive goes: the layer just over the deepest pane of glass
/// it's drawn after and over, or 0. depths of `levels` and more are left out.
pub(crate) fn layers(scene: &Scene, marks: &[(usize, DrawOrder, Bounds<ScaledPixels>)], levels: usize) -> Layers {
    let rects: Vec<[f64; 4]> = marks.iter().map(|(_, _, b)| [f64::from(b.origin.x.0), f64::from(b.origin.y.0), f64::from(b.size.width.0), f64::from(b.size.height.0)]).collect();
    let depth = depths(&rects);
    let glass: Vec<(DrawOrder, Bounds<ScaledPixels>, u8)> = marks.iter().zip(&depth).filter(|(_, d)| **d < levels).map(|((_, o, b), d)| (*o, *b, (*d + 1) as u8)).collect();
    let of = |order: DrawOrder, bounds: &Bounds<ScaledPixels>, mask: &Bounds<ScaledPixels>| -> u8 {
        let seen = bounds.intersect(mask);
        if seen.size.width.0 <= 0.0 || seen.size.height.0 <= 0.0 {
            return 0;
        }
        glass.iter().filter(|(at, g, _)| order >= *at && seen.intersects(g)).map(|(_, _, l)| *l).max().unwrap_or(0)
    };
    let mut l = Layers {
        shadows: scene.shadows.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        quads: scene.quads.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        paths: scene.paths.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        underlines: scene.underlines.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        monochrome_sprites: scene.monochrome_sprites.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        polychrome_sprites: scene.polychrome_sprites.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        surfaces: scene.surfaces.iter().map(|p| of(p.order, &p.bounds, &p.content_mask.bounds)).collect(),
        top: 0,
    };
    lift(scene, &mut l);
    l.top = [&l.shadows, &l.quads, &l.paths, &l.underlines, &l.monochrome_sprites, &l.polychrome_sprites, &l.surfaces]
        .iter()
        .flat_map(|v| v.iter())
        .copied()
        .max()
        .unwrap_or(0) as usize;
    l
}

/// keep the frame's order across layers: whatever is drawn after something it
/// overlaps must be in that thing's layer or one over it. (a menu over the
/// message box: its sheet reaches onto the glass and is lifted over it, so the
/// words on the sheet, though they don't reach the glass, must be lifted too.)
/// walked in draw order over a coarse grid of the frame, each cell holding the
/// highest layer drawn into it so far.
fn lift(scene: &Scene, l: &mut Layers) {
    if l.shadows.iter().chain(&l.quads).chain(&l.paths).chain(&l.underlines).chain(&l.monochrome_sprites).chain(&l.polychrome_sprites).chain(&l.surfaces).all(|x| *x == 0) {
        return;
    }
    const CELL: f32 = 32.0;
    // every primitive as (order, kind, index, its visible bounds).
    let mut all: Vec<(DrawOrder, u8, usize, Bounds<ScaledPixels>)> = Vec::new();
    let mut add = |kind: u8, i: usize, order: DrawOrder, b: &Bounds<ScaledPixels>, mask: &Bounds<ScaledPixels>| all.push((order, kind, i, b.intersect(mask)));
    for (i, p) in scene.shadows.iter().enumerate() { add(0, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.quads.iter().enumerate() { add(1, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.paths.iter().enumerate() { add(2, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.underlines.iter().enumerate() { add(3, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.monochrome_sprites.iter().enumerate() { add(4, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.polychrome_sprites.iter().enumerate() { add(5, i, p.order, &p.bounds, &p.content_mask.bounds) }
    for (i, p) in scene.surfaces.iter().enumerate() { add(6, i, p.order, &p.bounds, &p.content_mask.bounds) }
    all.sort_by_key(|p| p.0);
    let (mut cols, mut rows) = (1usize, 1usize);
    for (_, _, _, b) in &all {
        cols = cols.max(((b.origin.x.0 + b.size.width.0) / CELL).ceil().max(0.0) as usize + 1);
        rows = rows.max(((b.origin.y.0 + b.size.height.0) / CELL).ceil().max(0.0) as usize + 1);
    }
    let (cols, rows) = (cols.min(512), rows.min(512));
    let mut grid = vec![0u8; cols * rows];
    for (_, kind, i, b) in all {
        if b.size.width.0 <= 0.0 || b.size.height.0 <= 0.0 {
            continue;
        }
        let x0 = ((b.origin.x.0 / CELL).floor().max(0.0) as usize).min(cols - 1);
        let y0 = ((b.origin.y.0 / CELL).floor().max(0.0) as usize).min(rows - 1);
        let x1 = (((b.origin.x.0 + b.size.width.0) / CELL).ceil().max(0.0) as usize).clamp(x0 + 1, cols);
        let y1 = (((b.origin.y.0 + b.size.height.0) / CELL).ceil().max(0.0) as usize).clamp(y0 + 1, rows);
        let slot = match kind {
            0 => &mut l.shadows[i],
            1 => &mut l.quads[i],
            2 => &mut l.paths[i],
            3 => &mut l.underlines[i],
            4 => &mut l.monochrome_sprites[i],
            5 => &mut l.polychrome_sprites[i],
            _ => &mut l.surfaces[i],
        };
        let mut layer = *slot;
        for y in y0..y1 {
            for x in x0..x1 {
                layer = layer.max(grid[y * cols + x]);
            }
        }
        *slot = layer;
        if layer > 0 {
            for y in y0..y1 {
                for x in x0..x1 {
                    let c = &mut grid[y * cols + x];
                    *c = (*c).max(layer);
                }
            }
        }
    }
}

/// the runs of `range` whose primitives are in `layer`.
pub(crate) fn runs(of: &[u8], range: std::ops::Range<usize>, layer: u8) -> Vec<std::ops::Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for i in range.clone() {
        match (of.get(i).copied() == Some(layer), start) {
            (true, None) => start = Some(i),
            (false, Some(s)) => {
                out.push(s..i);
                start = None;
            }
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push(s..range.end);
    }
    out
}
