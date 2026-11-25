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
pub(crate) use tessel_paint::{Key, draw};


#[derive(Default)]
pub(crate) struct State {
    texture: Option<(Key, Arc<RenderImage>)>,
    /// drawn and waiting for a frame to hand it to the gpu.
    ready: Option<(Key, Vec<u8>)>,
    drawing: Option<Key>,
}

pub(crate) type Shared = Rc<RefCell<State>>;

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
            Some(Key { w: (w * scale).round() as u32, h: (h * scale).round() as u32, cell: (2.0 * scale).round().max(2.0) as u32, hue, dark, bg, felt: false, field: 0, fade: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 })
        })
    }

    pub(crate) fn mat_el(&self, radius: f32, cx: &mut Context<Self>) -> AnyElement {
        let dark = self.palette.get().dark;
        image_el(self.field_img.clone(), cx.entity().downgrade(), f32::from(theme::z(radius)), move |w, h, scale| {
            Some(Key { w: w.round() as u32, h: h.round() as u32, cell: (5.0 * scale).round().max(4.0) as u32, hue: 0.0, dark, bg: [0, 0, 0], felt: false, field: if dark { 1 } else { 2 }, fade: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 })
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
        image_el(self.felt.clone(), cx.entity().downgrade(), 0.0, move |w, h, _| Some(Key { w: w.round() as u32, h: h.round() as u32, cell: 1, hue: 0.0, dark, bg, felt: true, field: 0, fade: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 }))
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
    use super::{Key, draw};
    use tessel_paint::bayer;

    #[test]
    fn the_bayer_thresholds_are_each_used_once() {
        let mut seen: Vec<u32> = (0..8).flat_map(|y| (0..8).map(move |x| (bayer(x, y) * 64.0) as u32)).collect();
        seen.sort_unstable();
        assert_eq!(seen, (0..64).collect::<Vec<_>>());
    }

    /// speed is checked where the code is built as it ships; CI builds it
    /// unoptimized, to compile quickly, and there only correctness is.
    fn timed() -> bool {
        std::env::var_os("CI").is_none() && !cfg!(debug_assertions)
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
        let key = Key { w: 2880, h: 1800, cell: 4, hue: 0.6, dark: true, bg: [30, 30, 30], felt: false, field: 0, fade: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 };
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
        let key = Key { w: 1440, h: 900, cell: 1, hue: 0.0, dark: true, bg: [58, 18, 24], felt: true, field: 0, fade: false, meadow: false, frame: 0, frost: false, ox: 0, oy: 0, fw: 0, fh: 0 };
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
