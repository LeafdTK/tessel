//! one motion engine for the whole app.
//!
//! the rules, which are apple's:
//! - an animated value is a spring. it always starts from where the value is
//!   and how fast it is moving, so anything can be redirected or reversed
//!   mid-flight without a visible seam.
//! - no bounce, unless a gesture handed over momentum.
//! - only changes move. nothing in here loops.
//!
//! why our own state instead of the library's `spring()` helpers: those keep
//! their state inside the element tree and drop it the first frame an element
//! isn't drawn, and they can't be given a starting velocity. both rule out the
//! two things we need most, reversing mid-flight and settling with momentum.
//! the physics itself is the library's (`SpringConfig::step`, an exact
//! solution that does not depend on frame rate).

use gpui_kit::{
    AnyElement, App, Bounds, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement, LayoutId, Pixels,
    Point, SpringConfig, SpringState, Window,
};
use std::collections::HashMap;
use std::hash::Hash;
use std::time::Instant;

/// a spring described the way apple describes them: how long it takes to
/// settle (perceptually) and how much it overshoots.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Preset {
    pub duration: f32,
    pub bounce: f32,
}

impl Preset {
    /// apple's conversion, mass 1: stiffness = (2π/d)², damping = 4π(1−bounce)/d.
    pub fn config(self) -> SpringConfig {
        let tau = std::f32::consts::TAU;
        let stiffness = (tau / self.duration).powi(2);
        let damping = 2.0 * tau * (1.0 - self.bounce) / self.duration;
        SpringConfig::new(stiffness, damping, 1.0)
    }
}

/// following your hand while dragging. (apple's interactive spring.)
pub const TRACK: Preset = Preset { duration: 0.15, bounce: 0.14 };
/// small state changes.
pub const MICRO: Preset = Preset { duration: 0.20, bounce: 0.0 };
/// panels, rows entering and leaving, the prompt bar.
pub const PANEL: Preset = Preset { duration: 0.35, bounce: 0.0 };
/// the overview. (apple's `.smooth`.)
pub const SMOOTH: Preset = Preset { duration: 0.50, bounce: 0.0 };
/// settling after you let go with momentum. the only preset that overshoots.
pub const FLING: Preset = Preset { duration: 0.40, bounce: 0.15 };

/// one animated number.
#[derive(Clone, Copy, Debug)]
pub struct Spring {
    state: SpringState,
    target: f32,
    preset: Preset,
    /// how close counts as arrived, in the value's own units.
    epsilon: f32,
}

impl Spring {
    /// at rest at `value`. `epsilon` is "close enough": about a quarter of a
    /// point for positions, about a thousandth for 0..1 progress.
    pub fn at(value: f32, epsilon: f32) -> Self {
        Self { state: SpringState { position: value, velocity: 0.0 }, target: value, preset: PANEL, epsilon }
    }

    /// a position in points.
    pub fn points(value: f32) -> Self {
        Self::at(value, 0.25)
    }

    /// a 0..1 amount (opacity, how open something is).
    pub fn unit(value: f32) -> Self {
        Self::at(value, 0.002)
    }

    pub fn value(&self) -> f32 {
        self.state.position
    }

    pub fn target(&self) -> f32 {
        self.target
    }

    pub fn velocity(&self) -> f32 {
        self.state.velocity
    }

    /// head for a new target. position and velocity carry over, which is what
    /// makes a reversal smooth.
    pub fn to(&mut self, target: f32, preset: Preset) {
        self.target = target;
        self.preset = preset;
    }

    /// move where the value *is* without changing where it is going. this is
    /// how a row that was just re-laid-out somewhere else appears to start
    /// from its old place: shift by (old − new), then let it spring home.
    pub fn shift(&mut self, delta: f32) {
        self.state.position += delta;
    }

    /// let go with momentum: head for `target` carrying `velocity` (units per second).
    pub fn fling(&mut self, target: f32, velocity: f32) {
        self.target = target;
        self.preset = FLING;
        self.state.velocity = velocity;
    }

    /// be there now.
    pub fn snap(&mut self, value: f32) {
        self.state = SpringState { position: value, velocity: 0.0 };
        self.target = value;
    }

    pub fn is_moving(&self) -> bool {
        !self.preset.config().is_settled(self.state, self.target, self.epsilon)
    }

    /// advance by `dt` seconds. returns true while still moving. with
    /// `reduced` (the system's reduce-motion setting) it simply arrives.
    pub fn step(&mut self, dt: f32, reduced: bool) -> bool {
        if reduced || !self.is_moving() {
            // land exactly, so a settled value is the target and not a hair off it.
            self.snap(self.target);
            return false;
        }
        self.state = self.preset.config().step(self.state, self.target, dt);
        if self.is_moving() {
            true
        } else {
            self.snap(self.target);
            false
        }
    }
}

/// springs for a set of things that come and go, such as the rows of a list.
/// a key with no spring reads as 0, and springs resting at 0 are dropped, so
/// this only ever holds what is actually in motion.
#[derive(Debug)]
pub struct Springs<K> {
    live: HashMap<K, Spring>,
    epsilon: f32,
}

impl<K: Hash + Eq + Clone> Springs<K> {
    pub fn points() -> Self {
        Self { live: HashMap::new(), epsilon: 0.25 }
    }

    pub fn value(&self, key: &K) -> f32 {
        self.live.get(key).map_or(0.0, Spring::value)
    }

    /// make `key` appear displaced by `delta` and spring back to rest.
    pub fn shift(&mut self, key: K, delta: f32, preset: Preset) {
        if delta == 0.0 {
            return;
        }
        let spring = self.live.entry(key).or_insert_with(|| Spring::at(0.0, self.epsilon));
        spring.shift(delta);
        spring.to(0.0, preset);
    }

    pub fn is_moving(&self) -> bool {
        !self.live.is_empty()
    }

    /// advance everything. returns true while anything is still moving.
    pub fn step(&mut self, dt: f32, reduced: bool) -> bool {
        self.live.retain(|_, s| s.step(dt, reduced));
        !self.live.is_empty()
    }

    pub fn clear(&mut self) {
        self.live.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.live.len()
    }
}

/// turns "now" into seconds since the last animated frame.
#[derive(Debug, Default)]
pub struct Ticker {
    last: Option<Instant>,
}

impl Ticker {
    /// seconds since the previous call. the first frame of a run, and any
    /// frame after a long stall, counts as one normal frame: an animation
    /// must not leap forward because the app was briefly busy.
    pub fn dt(&mut self, now: Instant) -> f32 {
        let dt = match self.last {
            Some(last) => now.saturating_duration_since(last).as_secs_f32(),
            None => 1.0 / 120.0,
        };
        self.last = Some(now);
        dt.clamp(0.0, 1.0 / 20.0)
    }

    /// nothing is moving; forget the clock so the next run starts clean.
    pub fn rest(&mut self) {
        self.last = None;
    }
}

/// draws its child somewhere other than where layout put it.
///
/// moving something this way costs no layout pass: the child is laid out
/// once, in its real place, and only painted elsewhere. hit-testing follows
/// the painted position.
pub struct Fly {
    child: AnyElement,
    offset: Point<Pixels>,
}

/// paint `child` displaced by `offset` from its laid-out position.
pub fn fly(offset: Point<Pixels>, child: impl IntoElement) -> Fly {
    Fly { child: child.into_any_element(), offset }
}

impl IntoElement for Fly {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for Fly {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        window.with_element_offset(self.offset, |window| self.child.prepaint(window, cx));
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.paint(window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: f32 = 1.0 / 120.0;

    fn run(s: &mut Spring, seconds: f32) {
        let mut t = 0.0;
        while t < seconds {
            s.step(FRAME, false);
            t += FRAME;
        }
    }

    #[test]
    fn presets_match_apples_published_conversion() {
        // apple's own worked example: 0.5 s with bounce 0.3 is stiffness 157.9, damping 17.6.
        let c = Preset { duration: 0.5, bounce: 0.3 }.config();
        assert!((c.stiffness - 157.9).abs() < 0.1, "{}", c.stiffness);
        assert!((c.damping - 17.6).abs() < 0.1, "{}", c.damping);
        assert_eq!(c.mass, 1.0);
        // no bounce means critically damped: damping ratio exactly 1.
        for p in [MICRO, PANEL, SMOOTH] {
            assert!((p.config().canonical().1 - 1.0).abs() < 1e-4, "{p:?}");
        }
        assert!(FLING.config().canonical().1 < 1.0, "fling is the one that overshoots");
        assert!(TRACK.config().canonical().1 < 1.0);
    }

    #[test]
    fn arrives_exactly_and_then_stays_put() {
        let mut s = Spring::points(0.0);
        s.to(100.0, PANEL);
        assert!(s.is_moving());
        run(&mut s, 2.0);
        assert_eq!(s.value(), 100.0, "a settled spring is exactly on target");
        assert_eq!(s.velocity(), 0.0);
        assert!(!s.is_moving());
        assert!(!s.step(FRAME, false));
    }

    #[test]
    fn settles_in_about_the_time_the_preset_names() {
        for preset in [MICRO, PANEL, SMOOTH] {
            let mut s = Spring::points(0.0);
            s.to(100.0, preset);
            let mut t = 0.0;
            while (s.value() - 100.0).abs() > 1.0 {
                s.step(FRAME, false);
                t += FRAME;
                assert!(t < 5.0);
            }
            // "duration" is the perceptual one: within 1% of the way by roughly then.
            assert!(t < preset.duration * 1.6, "{preset:?} took {t}s to get within 1%");
            assert!(t > preset.duration * 0.5, "{preset:?} was done in {t}s, too abrupt");
        }
    }

    #[test]
    fn no_bounce_means_it_never_passes_the_target() {
        for preset in [MICRO, PANEL, SMOOTH] {
            let mut s = Spring::points(0.0);
            s.to(100.0, preset);
            let mut last = 0.0;
            for _ in 0..600 {
                s.step(FRAME, false);
                assert!(s.value() <= 100.0 + 1e-3, "{preset:?} overshot to {}", s.value());
                assert!(s.value() >= last - 1e-3, "{preset:?} went backwards");
                last = s.value();
            }
        }
    }

    #[test]
    fn fling_overshoots_a_little_and_comes_back() {
        let mut s = Spring::points(0.0);
        s.fling(100.0, 0.0);
        let mut peak: f32 = 0.0;
        for _ in 0..600 {
            s.step(FRAME, false);
            peak = peak.max(s.value());
        }
        assert!(peak > 100.2 && peak < 110.0, "peak {peak}");
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn redirecting_mid_flight_keeps_position_and_speed() {
        let mut s = Spring::points(0.0);
        s.to(100.0, PANEL);
        run(&mut s, 0.1);
        let (pos, vel) = (s.value(), s.velocity());
        assert!(pos > 5.0 && pos < 95.0 && vel > 0.0, "should be mid-flight: {pos} at {vel}/s");

        // reverse it.
        s.to(0.0, PANEL);
        assert_eq!((s.value(), s.velocity()), (pos, vel), "nothing jumps at the moment of reversal");
        // it is still carrying its speed, so it keeps going forward briefly before turning.
        s.step(FRAME, false);
        assert!(s.value() > pos, "momentum carries it on for a moment");
        assert!(s.velocity() < vel, "while it slows");
        run(&mut s, 2.0);
        assert_eq!(s.value(), 0.0);
    }

    #[test]
    fn the_same_motion_at_any_frame_rate() {
        let at = |fps: f32, seconds: f32| {
            let mut s = Spring::points(0.0);
            s.to(100.0, SMOOTH);
            let frames = (seconds * fps).round() as usize;
            for _ in 0..frames {
                s.step(1.0 / fps, false);
            }
            s.value()
        };
        let (slow, fast) = (at(30.0, 0.2), at(120.0, 0.2));
        assert!((slow - fast).abs() < 0.05, "30fps {slow} vs 120fps {fast}");
    }

    #[test]
    fn shift_starts_from_the_old_place_and_returns_home() {
        // a row that was at y=200 is now laid out at y=40: it should appear to start 160 below.
        let mut s = Spring::points(0.0);
        s.shift(160.0);
        s.to(0.0, PANEL);
        assert_eq!(s.value(), 160.0);
        assert_eq!(s.target(), 0.0);
        run(&mut s, 0.1);
        assert!(s.value() > 0.0 && s.value() < 160.0);
        // moved again before it got home: it continues from where it actually is.
        let here = s.value();
        s.shift(-44.0);
        assert_eq!(s.value(), here - 44.0);
        assert!(s.velocity() < 0.0, "still heading home");
        run(&mut s, 2.0);
        assert_eq!(s.value(), 0.0);
    }

    #[test]
    fn a_fling_carries_the_release_velocity() {
        let mut slow = Spring::points(0.0);
        let mut fast = Spring::points(0.0);
        slow.fling(100.0, 0.0);
        fast.fling(100.0, 2000.0);
        slow.step(FRAME, false);
        fast.step(FRAME, false);
        assert!(fast.value() > slow.value() + 10.0, "{} vs {}", fast.value(), slow.value());
    }

    #[test]
    fn reduce_motion_arrives_at_once() {
        let mut s = Spring::points(0.0);
        s.to(100.0, SMOOTH);
        assert!(!s.step(FRAME, true));
        assert_eq!(s.value(), 100.0);
    }

    #[test]
    fn springs_only_hold_what_is_moving() {
        let mut rows: Springs<u32> = Springs::points();
        assert_eq!(rows.value(&1), 0.0);
        assert!(!rows.is_moving());
        rows.shift(1, 120.0, PANEL);
        rows.shift(2, -44.0, PANEL);
        rows.shift(3, 0.0, PANEL);
        assert_eq!(rows.len(), 2, "a row that did not move has no spring");
        assert_eq!(rows.value(&1), 120.0);
        let mut frames = 0;
        while rows.step(FRAME, false) {
            frames += 1;
            assert!(frames < 2000);
        }
        assert_eq!(rows.len(), 0, "settled springs are dropped");
        assert_eq!(rows.value(&1), 0.0);

        rows.shift(1, 50.0, PANEL);
        assert!(!rows.step(FRAME, true), "reduce motion: nothing left to animate");
        assert_eq!(rows.value(&1), 0.0);
    }

    #[test]
    fn a_stall_does_not_make_the_animation_leap() {
        let mut t = Ticker::default();
        let start = Instant::now();
        assert!((t.dt(start) - 1.0 / 120.0).abs() < 1e-6, "the first frame counts as one frame");
        let next = start + std::time::Duration::from_millis(8);
        assert!((t.dt(next) - 0.008).abs() < 1e-4);
        // the app hung for two seconds.
        assert_eq!(t.dt(next + std::time::Duration::from_secs(2)), 1.0 / 20.0);
        // time appearing to run backwards is not a negative step.
        assert_eq!(t.dt(start), 0.0);
        t.rest();
        assert!((t.dt(start + std::time::Duration::from_secs(60)) - 1.0 / 120.0).abs() < 1e-6);
    }
}
