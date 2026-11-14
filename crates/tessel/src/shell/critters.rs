//! little animals made of dots, drifting behind the new-session card: seven
//! kinds, each a pixel sprite in two frames that moves its own way. a click
//! scatters them; koi come to it instead, as to food. which kinds are out is
//! the debug panel's (`debug.critters`), kept in `critters.json` beside the
//! layout file.
//!
//! cheap on purpose: they only move while they're painted and the window is
//! in front, at 30 frames a second, and a frame is a few hundred quads.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::switch::Switch;
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Butterfly,
    Jellyfish,
    Dragonfly,
    Koi,
    Frog,
    Crane,
    Crab,
}

pub(crate) const KINDS: [Kind; 7] = [Kind::Butterfly, Kind::Jellyfish, Kind::Dragonfly, Kind::Koi, Kind::Frog, Kind::Crane, Kind::Crab];

impl Kind {
    /// its name in `critters.json` and to `debug.critters`.
    pub(crate) fn id(self) -> &'static str {
        match self {
            Kind::Butterfly => "butterfly",
            Kind::Jellyfish => "jellyfish",
            Kind::Dragonfly => "dragonfly",
            Kind::Koi => "koi",
            Kind::Frog => "frog",
            Kind::Crane => "crane",
            Kind::Crab => "crab",
        }
    }

    pub(crate) fn from_id(id: &str) -> Option<Kind> {
        KINDS.into_iter().find(|k| k.id() == id)
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Butterfly => "Butterflies",
            Kind::Jellyfish => "Jellyfish",
            Kind::Dragonfly => "Dragonflies",
            Kind::Koi => "Koi",
            Kind::Frog => "Frogs",
            Kind::Crane => "Cranes",
            Kind::Crab => "Crabs",
        }
    }

    /// the sprite, facing up, in two frames. `.` is empty; letters are colours (`ink`).
    fn frames(self) -> [&'static [&'static str]; 2] {
        match self {
            Kind::Butterfly => [
                &["BB.....BB", "BBb...bBB", "BBBbdbBBB", ".BBBdBBB.", "..BBdBB..", ".BB.d.BB.", ".B..d..B."],
                &[".B.....B.", ".BB...BB.", "..BBdBB..", "..BBdBB..", "...BdB...", "..B.d.B..", "....d...."],
            ],
            Kind::Jellyfish => [
                &["..MMMMM..", ".MMmmmMM.", "MMmmmmmMM", "MMMMMMMMM", ".t.t.t.t.", ".t.t.t.t.", "t.t.t.t..", "..t.t.t.t"],
                &["...MMM...", ".MMmmmMM.", "MMmmmmmMM", ".MMMMMMM.", "..t.t.t..", ".t.t.t.t.", "..t.t.t.t", ".t.t.t..."],
            ],
            Kind::Dragonfly => [
                &["....e....", "...GGG...", "WWW.G.WWW", ".WWWGWWW.", "WWW.G.WWW", ".W..G..W.", "....G....", "....G....", "....G....", "....g...."],
                &["....e....", "...GGG...", ".WW.G.WW.", "WWWWGWWWW", ".WW.G.WW.", "W...G...W", "....G....", "....G....", "....G....", "....g...."],
            ],
            Kind::Koi => [
                &["..WWW..", ".WRRWW.", ".WRWRW.", ".WWRRW.", "..RWW..", "..WRW..", "...W...", "..W.W..", ".W...W."],
                &["..WWW..", ".WRRWW.", ".WRWRW.", ".WWRRW.", "..RWW..", "..WRW..", "...W...", "...WW..", "....WW."],
            ],
            Kind::Frog => [
                &["G.......G", "GG.GGG.GG", ".GGeGeGG.", "..GGGGG..", "..GgggG..", ".GGGGGGG.", "GG.....GG", "G.......G"],
                &["...GGG...", "G.GeGeG.G", ".GGGGGGG.", "..GgggG..", "..GGGGG..", "..GG.GG..", ".GG...GG.", "........."],
            ],
            Kind::Crane => [
                &["....r....", "....W....", "....W....", "WWWWWWWWW", ".kWWWWWk.", "..k.W.k..", "....W....", "...W.W..."],
                &["....r....", "....W....", "W...W...W", ".WWWWWWW.", "..kWWWk..", "....W....", "....W....", "...W.W..."],
            ],
            Kind::Crab => [
                &["R.......R", "RR.....RR", ".R.e.e.R.", "..RRRRR..", ".RRRRRRR.", "R.RRRRR.R", ".R.R.R.R."],
                &[".R.....R.", "RR.....RR", ".R.e.e.R.", "..RRRRR..", ".RRRRRRR.", ".RRRRRRR.", "R.R.R.R.R"],
            ],
        }
    }

    /// what colour a sprite's letter is, by kind; none for a gap.
    fn ink(self, c: char, dark: bool) -> Option<Hsla> {
        let l = |light: f32, deep: f32| if dark { light } else { deep };
        let h = |hue: f32, s: f32, lit: f32| Some(gpui_kit::hsla(hue, s, lit, 1.0));
        match (self, c) {
            (_, '.') => None,
            (_, 'e') => h(0.0, 0.0, l(0.95, 0.12)),
            (Kind::Butterfly, 'B') => h(0.59, 0.85, l(0.6, 0.5)),
            (Kind::Butterfly, 'b') => h(0.56, 0.9, l(0.75, 0.62)),
            (Kind::Butterfly, 'd') => h(0.62, 0.4, l(0.35, 0.25)),
            (Kind::Jellyfish, 'M') => h(0.86, 0.7, l(0.62, 0.5)),
            (Kind::Jellyfish, 'm') => h(0.88, 0.8, l(0.78, 0.66)),
            (Kind::Jellyfish, 't') => h(0.84, 0.5, l(0.55, 0.55)),
            (Kind::Dragonfly, 'G') => h(0.38, 0.75, l(0.5, 0.36)),
            (Kind::Dragonfly, 'g') => h(0.4, 0.6, l(0.4, 0.3)),
            (Kind::Dragonfly, 'W') => h(0.45, 0.5, l(0.72, 0.6)),
            (Kind::Koi, 'W') => h(0.08, 0.15, l(0.94, 0.78)),
            (Kind::Koi, 'R') => h(0.02, 0.85, l(0.55, 0.48)),
            (Kind::Frog, 'G') => h(0.31, 0.65, l(0.48, 0.38)),
            (Kind::Frog, 'g') => h(0.2, 0.7, l(0.7, 0.55)),
            (Kind::Crane, 'W') => h(0.6, 0.05, l(0.92, 0.55)),
            (Kind::Crane, 'k') => h(0.6, 0.1, l(0.35, 0.25)),
            (Kind::Crane, 'r') => h(0.0, 0.85, l(0.55, 0.5)),
            (Kind::Crab, 'R') => h(0.03, 0.8, l(0.55, 0.48)),
            _ => None,
        }
    }

    /// how many there are, and how big a dot is (points).
    fn crowd(self) -> (usize, f32) {
        match self {
            Kind::Butterfly => (6, 4.8),
            Kind::Jellyfish => (4, 5.0),
            Kind::Dragonfly => (4, 4.2),
            Kind::Koi => (7, 4.8),
            Kind::Frog => (3, 4.8),
            Kind::Crane => (3, 5.6),
            Kind::Crab => (4, 4.8),
        }
    }

    /// how fast it goes, points a second.
    fn speed(self) -> f32 {
        match self {
            Kind::Butterfly => 38.,
            Kind::Jellyfish => 14.,
            Kind::Dragonfly => 90.,
            Kind::Koi => 30.,
            Kind::Frog => 0.,
            Kind::Crane => 26.,
            Kind::Crab => 22.,
        }
    }
}

struct Critter {
    kind: Kind,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    /// where it faces, radians (0 is up).
    heading: f32,
    /// its own clock, so they don't move in step.
    t: f32,
    seed: f32,
}

/// what's swimming, and where.
pub(crate) struct Sim {
    pub on: Vec<Kind>,
    critters: Vec<Critter>,
    food: Vec<(f32, f32, f32)>,
    size: (f32, f32),
    /// the card they keep off, in the same space.
    pub avoid: Option<Bounds<Pixels>>,
    origin: Point<Pixels>,
    last: Option<Instant>,
    seen: Option<Instant>,
    rng: u32,
}

impl Sim {
    pub(crate) fn new(on: Vec<Kind>) -> Self {
        Self { on, critters: Vec::new(), food: Vec::new(), size: (0., 0.), avoid: None, origin: point(px(0.), px(0.)), last: None, seen: None, rng: 0x9E37_79B9 }
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        (self.rng % 100_000) as f32 / 100_000.0
    }

    /// painted lately, so worth moving.
    pub(crate) fn wanted(&self) -> bool {
        !self.on.is_empty() && self.seen.is_some_and(|s| s.elapsed().as_millis() < 250)
    }

    /// put out the kinds that are on and aren't out yet, and take in the rest.
    fn populate(&mut self) {
        self.critters.retain(|c| self.on.contains(&c.kind));
        let (w, h) = self.size;
        if w < 10. || h < 10. {
            return;
        }
        for kind in self.on.clone() {
            if self.critters.iter().any(|c| c.kind == kind) {
                continue;
            }
            for _ in 0..kind.crowd().0 {
                let (x, y, a, s) = (self.rand() * w, self.rand() * h, self.rand() * std::f32::consts::TAU, self.rand());
                self.critters.push(Critter { kind, x, y, vx: 0., vy: 0., heading: a, t: s * 10., seed: s });
            }
        }
    }

    /// a click at (x, y), in the window: they scatter, and koi come to it.
    pub(crate) fn poke(&mut self, at: Point<Pixels>) {
        let (x, y) = (f32::from(at.x - self.origin.x), f32::from(at.y - self.origin.y));
        if self.critters.iter().any(|c| c.kind == Kind::Koi) {
            self.food.push((x, y, 6.0));
        }
        for c in &mut self.critters {
            if c.kind == Kind::Koi {
                continue;
            }
            let (dx, dy) = (c.x - x, c.y - y);
            let d = (dx * dx + dy * dy).sqrt().max(1.);
            let push = (2600. / d).min(420.);
            c.vx += dx / d * push;
            c.vy += dy / d * push;
            c.heading = dx.atan2(-dy);
        }
    }

    /// move everyone on by however long it's been.
    pub(crate) fn step(&mut self) {
        let now = Instant::now();
        let dt = self.last.map_or(0.033, |l| (now - l).as_secs_f32()).min(0.1);
        self.last = Some(now);
        self.populate();
        let (w, h) = self.size;
        let avoid = self.avoid.map(|b| {
            let (x, y) = (f32::from(b.origin.x - self.origin.x), f32::from(b.origin.y - self.origin.y));
            (x - 24., y - 24., x + f32::from(b.size.width) + 24., y + f32::from(b.size.height) + 24.)
        });
        let food = self.food.clone();
        for c in &mut self.critters {
            c.t += dt;
            let speed = c.kind.speed();
            // each kind's own way of going: where it wants to head, and how hard.
            let wobble = (c.t * 0.7 + c.seed * 40.).sin() * 0.9 + (c.t * 1.9 + c.seed * 13.).sin() * 0.4;
            let mut want = c.heading + wobble * dt * 2.0;
            let mut go = speed;
            match c.kind {
                Kind::Jellyfish => {
                    want = (c.seed - 0.5) * 0.6 + wobble * 0.15;
                    go = speed * (0.4 + (c.t * 2.2).sin().max(0.) * 1.6);
                }
                Kind::Dragonfly => {
                    let burst = ((c.t * 0.9 + c.seed * 7.).sin() > 0.6) as u8 as f32;
                    go = speed * (0.15 + burst);
                    want = c.heading + wobble * dt * 6.0;
                }
                Kind::Frog => {
                    let hop = (c.t * 0.6 + c.seed * 9.).sin();
                    go = if hop > 0.93 { 160. } else { 0. };
                    want = c.heading + wobble * dt * 3.0;
                }
                Kind::Crab => {
                    go = speed * (((c.t * 1.3 + c.seed * 5.).sin() > 0.0) as u8 as f32);
                    want = std::f32::consts::FRAC_PI_2 * if (c.t * 0.25 + c.seed * 3.).sin() > 0.0 { 1. } else { -1. };
                }
                Kind::Koi => {
                    if let Some((fx, fy, _)) = food.iter().min_by(|a, b| ((a.0 - c.x).powi(2) + (a.1 - c.y).powi(2)).total_cmp(&((b.0 - c.x).powi(2) + (b.1 - c.y).powi(2)))) {
                        want = (fx - c.x).atan2(-(fy - c.y));
                        go = speed * 2.4;
                    }
                }
                _ => {}
            }
            // off the card, and back from the edges.
            if let Some((x0, y0, x1, y1)) = avoid
                && c.x > x0
                && c.x < x1
                && c.y > y0
                && c.y < y1
            {
                let (cx, cy) = ((x0 + x1) / 2., (y0 + y1) / 2.);
                want = (c.x - cx).atan2(-(c.y - cy));
                go = go.max(speed * 1.5).max(30.);
            }
            let margin = 30.;
            if c.x < margin || c.x > w - margin || c.y < margin || c.y > h - margin {
                want = (w / 2. - c.x).atan2(-(h / 2. - c.y));
            }
            // turn toward it, smoothly.
            let mut d = want - c.heading;
            while d > std::f32::consts::PI {
                d -= std::f32::consts::TAU;
            }
            while d < -std::f32::consts::PI {
                d += std::f32::consts::TAU;
            }
            if c.kind != Kind::Crab {
                c.heading += d * (dt * 2.5).min(1.);
            }
            let (dir_x, dir_y) = if c.kind == Kind::Crab { (want.sin(), -want.cos()) } else { (c.heading.sin(), -c.heading.cos()) };
            // a scatter's push wears off.
            let fade = (-dt * 2.2).exp();
            c.vx *= fade;
            c.vy *= fade;
            c.x += (dir_x * go + c.vx) * dt;
            c.y += (dir_y * go + c.vy) * dt;
            c.x = c.x.clamp(-20., w + 20.);
            c.y = c.y.clamp(-20., h + 20.);
            if c.kind == Kind::Crab {
                c.heading = 0.;
            }
        }
        // food goes when a koi gets to it, or after a while.
        let koi: Vec<(f32, f32)> = self.critters.iter().filter(|c| c.kind == Kind::Koi).map(|c| (c.x, c.y)).collect();
        for f in &mut self.food {
            f.2 -= dt;
            if koi.iter().any(|(x, y)| (x - f.0).powi(2) + (y - f.1).powi(2) < 100.) {
                f.2 = 0.;
            }
        }
        self.food.retain(|f| f.2 > 0.);
    }

    /// draw them into `bounds`.
    pub(crate) fn paint(&mut self, bounds: Bounds<Pixels>, dark: bool, window: &mut Window) {
        self.seen = Some(Instant::now());
        self.origin = bounds.origin;
        self.size = (f32::from(bounds.size.width), f32::from(bounds.size.height));
        self.populate();
        let o = bounds.origin;
        for (fx, fy, life) in &self.food {
            let a = (life / 6.).clamp(0., 1.);
            let s = px(3.);
            window.paint_quad(gpui_kit::fill(Bounds::new(point(o.x + px(*fx) - s / 2., o.y + px(*fy) - s / 2.), size(s, s)), gpui_kit::hsla(0.12, 0.6, 0.6, a)));
        }
        for c in &self.critters {
            let frames = c.kind.frames();
            let flap = match c.kind {
                Kind::Butterfly => 7.,
                Kind::Dragonfly => 14.,
                Kind::Jellyfish => 1.8,
                Kind::Koi => 2.5,
                Kind::Crab => 4.,
                Kind::Crane => 1.2,
                Kind::Frog => 0.,
            };
            let frame = if flap > 0. && (c.t * flap + c.seed * 6.).sin() > 0. { frames[1] } else { frames[0] };
            let cell = c.kind.crowd().1;
            let dot = cell * 0.78;
            let rows = frame.len() as f32;
            let cols = frame[0].len() as f32;
            let (sin, cos) = c.heading.sin_cos();
            for (r, line) in frame.iter().enumerate() {
                for (k, ch) in line.chars().enumerate() {
                    let Some(ink) = c.kind.ink(ch, dark) else { continue };
                    // the dot's place on the sprite, centred, turned to where it faces.
                    let (lx, ly) = ((k as f32 - (cols - 1.) / 2.) * cell, (r as f32 - (rows - 1.) / 2.) * cell);
                    let (rx, ry) = (lx * cos - ly * sin, lx * sin + ly * cos);
                    let at = point(o.x + px(c.x + rx - dot / 2.), o.y + px(c.y + ry - dot / 2.));
                    window.paint_quad(gpui_kit::fill(Bounds::new(at, size(px(dot), px(dot))), ink.opacity(if dark { 0.9 } else { 0.8 })).corner_radii(px(dot * 0.25)));
                }
            }
        }
    }
}

pub(crate) type Shared = Rc<RefCell<Sim>>;

/// which kinds are out, from `critters.json` beside the layout; all of them
/// if there's none.
pub(crate) fn load(dir: Option<&std::path::Path>) -> Vec<Kind> {
    dir.map(|d| d.join("critters.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok())
        .map(|ids| ids.iter().filter_map(|i| Kind::from_id(i)).collect())
        .unwrap_or_else(|| KINDS.to_vec())
}

pub(crate) fn save(dir: Option<&std::path::Path>, on: &[Kind]) {
    if let Some(d) = dir {
        let ids: Vec<&str> = on.iter().map(|k| k.id()).collect();
        let _ = std::fs::write(d.join("critters.json"), serde_json::to_string(&ids).unwrap_or_default());
    }
}

impl Shell {
    fn critters_dir(&self) -> Option<std::path::PathBuf> {
        self.ui_path.as_ref().and_then(|p| p.parent()).map(std::path::Path::to_path_buf)
    }

    /// the animals, filling their parent; clicks on them scatter them.
    pub(crate) fn critters_el(&self) -> AnyElement {
        let p = self.palette.get();
        let sim = self.critters.clone();
        let dark = p.dark;
        let poke = self.critters.clone();
        div()
            .id("critters")
            .absolute()
            .inset_0()
            .on_mouse_down(MouseButton::Left, move |ev: &MouseDownEvent, _, _| {
                let mut s = poke.borrow_mut();
                // a click on the card is the card's.
                if s.avoid.is_some_and(|b| b.contains(&ev.position)) {
                    return;
                }
                s.poke(ev.position);
            })
            .child(canvas(|_, _, _| {}, move |b, _, window, _| sim.borrow_mut().paint(b, dark, window)).absolute().inset_0())
            .into_any_element()
    }

    /// records where the new-session card is, for them to keep off it.
    pub(crate) fn critters_card(&self) -> AnyElement {
        let sim = self.critters.clone();
        canvas(move |b, _, _| sim.borrow_mut().avoid = Some(b), |_, _, _, _| {}).absolute().inset_0().into_any_element()
    }

    /// keep them moving while they're shown: 30 frames a second, only while
    /// painted and the window is in front. started by `render` when the
    /// new-session screen is up; it stops itself a moment after it isn't.
    pub(crate) fn critters_ensure(&mut self, cx: &mut Context<Self>) {
        // tests drive time themselves: a timer always waiting would move their clock.
        if cfg!(test) || self.critters_running || self.critters.borrow().on.is_empty() {
            return;
        }
        self.critters_running = true;
        let sim = self.critters.clone();
        cx.spawn(async move |this, cx| {
            let mut idle = 0;
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(33)).await;
                let active = this.update(cx, |this, _| this.window_active).unwrap_or(false);
                if !sim.borrow().wanted() || !active {
                    sim.borrow_mut().last = None;
                    idle += 1;
                    if idle > 30 {
                        break;
                    }
                    continue;
                }
                idle = 0;
                sim.borrow_mut().step();
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    return;
                }
            }
            let _ = this.update(cx, |this, _| this.critters_running = false);
        })
        .detach();
    }

    pub(crate) fn critters_toggle(&mut self, kind: Kind, cx: &mut Context<Self>) {
        let on = {
            let mut s = self.critters.borrow_mut();
            if let Some(i) = s.on.iter().position(|k| *k == kind) {
                s.on.remove(i);
            } else {
                s.on.push(kind);
            }
            s.on.clone()
        };
        save(self.critters_dir().as_deref(), &on);
        cx.notify();
    }

    pub(crate) fn critters_set(&mut self, on: Vec<Kind>, cx: &mut Context<Self>) {
        self.critters.borrow_mut().on = on.clone();
        save(self.critters_dir().as_deref(), &on);
        cx.notify();
    }

    /// the way to the debug panel: a quiet fish in the bottom right corner.
    pub(crate) fn critters_button(&self, cx: &mut Context<Self>) -> AnyElement {
        div()
            .absolute()
            .bottom(px(12.))
            .right(px(12.))
            .child(
                Button::new("critters-open")
                    .ghost()
                    .xsmall()
                    .icon(Icon::empty().path("icons/fish.svg"))
                    .tooltip("Critters")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.critters_panel_open = !this.critters_panel_open;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// the debug panel: a switch for each kind, over the top right of the new-session screen.
    pub(crate) fn critters_panel(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.critters_panel_open {
            return None;
        }
        let p = self.palette.get();
        let on = self.critters.borrow().on.clone();
        let mut list = v_flex().gap(px(2.));
        for kind in KINDS {
            let is_on = on.contains(&kind);
            list = list.child(
                h_flex()
                    .h(px(30.))
                    .px(px(10.))
                    .gap_2()
                    .child(div().flex_1().set(Type::Callout).text_color(p.label).child(kind.name()))
                    .child(Switch::new(SharedString::from(format!("critters-{}", kind.name()))).checked(is_on).on_click(cx.listener(move |this, _, _, cx| this.critters_toggle(kind, cx)))),
            );
        }
        Some(
            v_flex()
                .id("critters-panel")
                .test_support()
                .absolute()
                .top(px(12.))
                .right(px(12.))
                .w(px(220.))
                .py(px(6.))
                .rounded(px(theme::R_CARD))
                .border_1()
                .border_color(p.separator)
                .bg(p.raised)
                .shadow_lg()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(
                    h_flex()
                        .px(px(10.))
                        .pb(px(4.))
                        .child(div().flex_1().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child("Critters"))
                        .child(Button::new("critters-all").ghost().xsmall().label("All").on_click(cx.listener(|this, _, _, cx| this.critters_set(KINDS.to_vec(), cx))))
                        .child(Button::new("critters-none").ghost().xsmall().label("None").on_click(cx.listener(|this, _, _, cx| this.critters_set(Vec::new(), cx))))
                        .child(Button::new("critters-close").ghost().xsmall().icon(Icon::empty().path("icons/x.svg")).on_click(cx.listener(|this, _, _, cx| {
                            this.critters_panel_open = false;
                            cx.notify();
                        }))),
                )
                .child(list)
                .into_any_element(),
        )
    }
}
