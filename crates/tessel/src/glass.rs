//! liquid glass behind the window: on macOS 26 and later, the glass material
//! is apple's own (`NSGlassEffectView`), the desktop seen through it bent and
//! lit as the system's glass is. it sits under tessel's view, filling the
//! window; the panes are drawn over it as they always are.
//!
//! where the system has no liquid glass, `set` says so and the window keeps
//! the plain blur gpui gives it.

/// put liquid glass behind `view`'s window (`on`), or take it away. whether the
/// system has it: false means the window should blur the old way.
#[cfg(target_os = "macos")]
pub fn set(view: usize, on: bool, dark: bool) -> bool {
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{msg_send, sel};

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct(
            "CGRect",
            &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])],
        );
    }

    let Some(glass) = AnyClass::get(c"NSGlassEffectView") else { return false };
    let view = view as *mut AnyObject;
    if view.is_null() {
        return false;
    }
    unsafe {
        let window: *mut AnyObject = msg_send![view, window];
        if window.is_null() {
            return false;
        }
        let content: *mut AnyObject = msg_send![window, contentView];
        if content.is_null() {
            return false;
        }
        // ours, if it's there already: found by the identifier it was given.
        let mark: Retained<AnyObject> = {
            let s: *mut AnyObject = msg_send![AnyClass::get(c"NSString").expect("Foundation"), stringWithUTF8String: c"tessel-liquid-glass".as_ptr()];
            Retained::retain(s).expect("a string")
        };
        let subviews: *mut AnyObject = msg_send![content, subviews];
        let n: usize = msg_send![subviews, count];
        let mut ours: *mut AnyObject = std::ptr::null_mut();
        for i in 0..n {
            let v: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let id: *mut AnyObject = msg_send![v, identifier];
            if !id.is_null() {
                let same: Bool = msg_send![id, isEqualToString: &*mark];
                if same.as_bool() {
                    ours = v;
                    break;
                }
            }
        }
        if !on {
            if !ours.is_null() {
                let _: () = msg_send![ours, removeFromSuperview];
            }
            return true;
        }
        if ours.is_null() {
            let frame: Rect = msg_send![content, bounds];
            let v: *mut AnyObject = msg_send![glass, alloc];
            let v: *mut AnyObject = msg_send![v, initWithFrame: frame];
            if v.is_null() {
                return false;
            }
            const WIDTH_SIZABLE: usize = 2;
            const HEIGHT_SIZABLE: usize = 16;
            let _: () = msg_send![v, setAutoresizingMask: WIDTH_SIZABLE | HEIGHT_SIZABLE];
            let _: () = msg_send![v, setIdentifier: &*mark];
            const BELOW: isize = -1;
            let _: () = msg_send![content, addSubview: v, positioned: BELOW, relativeTo: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![v, release];
            ours = v;
        }
        // the regular glass, a touch of the theme's own dark or light in it.
        let has_tint: Bool = msg_send![ours, respondsToSelector: sel!(setTintColor:)];
        if has_tint.as_bool() {
            let (w, a) = if dark { (0.06, 0.35) } else { (0.98, 0.25) };
            let color: *mut AnyObject = msg_send![AnyClass::get(c"NSColor").expect("AppKit"), colorWithWhite: w, alpha: a];
            let _: () = msg_send![ours, setTintColor: color];
        }
        true
    }
}

#[cfg(not(target_os = "macos"))]
pub fn set(_view: usize, _on: bool, _dark: bool) -> bool {
    false
}


use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};

static LIQUID: AtomicBool = AtomicBool::new(false);
static DARK: AtomicBool = AtomicBool::new(true);

/// the window's surfaces are liquid glass now (glass material, and the system has it).
pub fn liquid() -> bool {
    LIQUID.load(Ordering::Relaxed)
}

pub fn set_liquid(on: bool, dark: bool) {
    if std::env::var_os("TESSEL_GLASS_LOG").is_some() {
        eprintln!("tessel glass: liquid {on}, dark {dark}");
    }
    LIQUID.store(on, Ordering::Relaxed);
    DARK.store(dark, Ordering::Relaxed);
}

/// a surface that's glass: where it is on the window (points, from the top
/// left), and how round its corners are.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spot {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub radius: f32,
}

thread_local! {
    /// the spots this frame painted, in order.
    static SPOTS: RefCell<Vec<Spot>> = const { RefCell::new(Vec::new()) };
    /// what was last put on the window: nothing's touched if it's the same.
    static SHOWN: RefCell<Vec<Spot>> = const { RefCell::new(Vec::new()) };
}

/// note a glass surface, as it's painted.
pub fn spot(s: Spot) {
    SPOTS.with(|v| v.borrow_mut().push(s));
}

/// put a pane of liquid glass under each surface noted this frame (and take
/// away the ones no surface needs): called once, after everything is painted.
pub fn sync(view: usize) {
    let spots = SPOTS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let spots = if liquid() { spots } else { Vec::new() };
    if SHOWN.with(|s| *s.borrow() == spots) {
        return;
    }
    SHOWN.with(|s| *s.borrow_mut() = spots.clone());
    if std::env::var_os("TESSEL_GLASS_LOG").is_some() {
        eprintln!("tessel glass: {} spots {:?}", spots.len(), spots.first());
    }
    #[cfg(target_os = "macos")]
    place(view, &spots, DARK.load(Ordering::Relaxed));
    #[cfg(not(target_os = "macos"))]
    let _ = view;
}

#[cfg(target_os = "macos")]
fn place(view: usize, spots: &[Spot], dark: bool) {
    use objc2::encode::{Encode, Encoding};
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl Encode for Rect {
        const ENCODING: Encoding = Encoding::Struct("CGRect", &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])]);
    }
    let Some(glass) = AnyClass::get(c"NSGlassEffectView") else { return };
    let ours = view as *mut AnyObject;
    if ours.is_null() {
        return;
    }
    unsafe {
        let content: *mut AnyObject = msg_send![ours, superview];
        if content.is_null() {
            return;
        }
        let nsstring = AnyClass::get(c"NSString").expect("Foundation");
        let mark: *mut AnyObject = msg_send![nsstring, stringWithUTF8String: c"tessel-glass-spot".as_ptr()];
        // the spots on the window now, oldest first.
        let subviews: *mut AnyObject = msg_send![content, subviews];
        let n: usize = msg_send![subviews, count];
        let mut have: Vec<*mut AnyObject> = Vec::new();
        for i in 0..n {
            let v: *mut AnyObject = msg_send![subviews, objectAtIndex: i];
            let id: *mut AnyObject = msg_send![v, identifier];
            if !id.is_null() {
                let same: Bool = msg_send![id, isEqualToString: mark];
                if same.as_bool() {
                    have.push(v);
                }
            }
        }
        let bounds: Rect = msg_send![content, bounds];
        let flipped: Bool = msg_send![content, isFlipped];
        let look: *mut AnyObject = {
            let name: *mut AnyObject = msg_send![nsstring, stringWithUTF8String: if dark { c"NSAppearanceNameDarkAqua".as_ptr() } else { c"NSAppearanceNameAqua".as_ptr() }];
            msg_send![AnyClass::get(c"NSAppearance").expect("AppKit"), appearanceNamed: name]
        };
        for (i, s) in spots.iter().enumerate() {
            let v = match have.get(i) {
                Some(v) => *v,
                None => {
                    let v: *mut AnyObject = msg_send![glass, alloc];
                    let v: *mut AnyObject = msg_send![v, initWithFrame: Rect { x: 0., y: 0., w: 1., h: 1. }];
                    if v.is_null() {
                        return;
                    }
                    let _: () = msg_send![v, setIdentifier: mark];
                    // just under tessel's own view: over the window's glass, under everything drawn.
                    const BELOW: isize = -1;
                    let _: () = msg_send![content, addSubview: v, positioned: BELOW, relativeTo: ours];
                    let _: () = msg_send![v, release];
                    v
                }
            };
            let y = if flipped.as_bool() { f64::from(s.y) } else { bounds.h - f64::from(s.y) - f64::from(s.h) };
            let _: () = msg_send![v, setFrame: Rect { x: f64::from(s.x), y, w: f64::from(s.w), h: f64::from(s.h) }];
            let _: () = msg_send![v, setCornerRadius: f64::from(s.radius)];
            let _: () = msg_send![v, setAppearance: look];
        }
        for v in have.iter().skip(spots.len()) {
            let _: () = msg_send![*v, removeFromSuperview];
        }
    }
}
