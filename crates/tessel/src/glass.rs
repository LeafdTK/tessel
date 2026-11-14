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
    /// glass with a colour in it; none: clear.
    pub tint: Option<gpui_kit::Hsla>,
}

thread_local! {
    /// the spots this frame painted, in order.
    static SPOTS: RefCell<Vec<Spot>> = const { RefCell::new(Vec::new()) };
    /// what was last put on the window: nothing's touched if it's the same.
    static SHOWN: RefCell<Vec<Spot>> = const { RefCell::new(Vec::new()) };
    /// the panes of glass floating over tessel's drawing this frame, in the
    /// order they're painted, and the last ones shown.
    static FLOATS: RefCell<Vec<Spot>> = const { RefCell::new(Vec::new()) };
    static FLOATS_SHOWN: RefCell<Option<Vec<Spot>>> = const { RefCell::new(None) };
}

/// note a glass surface, as it's painted.
pub fn spot(s: Spot) {
    SPOTS.with(|v| v.borrow_mut().push(s));
}

/// most panes of floating glass a window has at once.
pub const MAX_FLOATS: usize = 48;

thread_local! {
    /// device pixels a point, for working out depths as the renderer does.
    static SCALE: std::cell::Cell<f64> = const { std::cell::Cell::new(2.0) };
}

/// the window's scale, as glass is painted.
pub fn set_scale(scale: f32) {
    SCALE.with(|s| s.set(f64::from(scale)));
}

/// note a pane of glass that floats over what's drawn before it (a sheet, the
/// message box): what's under it is bent through it, live. which one it is,
/// for its mark; none once there are as many as a window takes.
pub fn float(s: Spot) -> Option<usize> {
    FLOATS.with(|f| {
        let mut f = f.borrow_mut();
        (f.len() < MAX_FLOATS).then(|| {
            f.push(s);
            f.len() - 1
        })
    })
}

/// the border colour of the mark painted where floating glass `i` is: the
/// renderer draws what's painted after it, over it, above the glass.
pub fn mark(i: usize) -> gpui_kit::Hsla {
    #[cfg(target_os = "macos")]
    {
        gpui_apple::glass_split::mark(i)
    }
    #[cfg(not(target_os = "macos"))]
    gpui_kit::Hsla { h: 0.618_034, s: 0.271_828, l: (i as f32 + 0.5) / 64.0, a: 0.0 }
}

/// the system has liquid glass at all (macOS 26 on).
pub fn available() -> bool {
    #[cfg(target_os = "macos")]
    {
        static HAS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *HAS.get_or_init(|| objc2::runtime::AnyClass::get(c"NSGlassEffectView").is_some())
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// put a pane of liquid glass under each surface noted this frame (and take
/// away the ones no surface needs), and the floating one over what's under
/// it: called once, after everything is painted.
pub fn sync(view: usize) {
    let spots = SPOTS.with(|v| std::mem::take(&mut *v.borrow_mut()));
    let floats = FLOATS.with(|f| std::mem::take(&mut *f.borrow_mut()));
    if !available() {
        return;
    }
    if FLOATS_SHOWN.with(|s| s.borrow().as_ref() != Some(&floats)) {
        #[cfg(target_os = "macos")]
        place_floats(view, &floats, DARK.load(Ordering::Relaxed));
        FLOATS_SHOWN.with(|s| *s.borrow_mut() = Some(floats.clone()));
    }
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
#[repr(C)]
#[derive(Clone, Copy)]
struct NsRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for NsRect {
    const ENCODING: objc2::encode::Encoding = {
        use objc2::encode::Encoding;
        Encoding::Struct("CGRect", &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])])
    };
}

/// a view that clicks go through: the glass and the layer over it are only
/// pictures; tessel's own view under them takes every event.
#[cfg(target_os = "macos")]
fn passthrough_class() -> &'static objc2::runtime::AnyClass {
    use objc2::encode::{Encode, Encoding};
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    unsafe impl Encode for Point {
        const ENCODING: Encoding = Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    extern "C-unwind" fn hit_test(_this: &AnyObject, _sel: Sel, _at: Point) -> *mut AnyObject {
        std::ptr::null_mut()
    }
    static CLASS: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    let class = *CLASS.get_or_init(|| {
        if let Some(c) = AnyClass::get(c"TesselPassthroughView") {
            return c as *const AnyClass as usize;
        }
        let mut b = ClassBuilder::new(c"TesselPassthroughView", AnyClass::get(c"NSView").expect("AppKit")).expect("a new class");
        unsafe { b.add_method(objc2::sel!(hitTest:), hit_test as extern "C-unwind" fn(_, _, _) -> _) };
        b.register() as *const AnyClass as usize
    });
    unsafe { &*(class as *const AnyClass) }
}

/// clip a pane of glass to its rounded corners: the glass rounds its rim, but
/// a tint fills the whole rectangle unless the layer's cut to it too.
#[cfg(target_os = "macos")]
unsafe fn round(v: *mut objc2::runtime::AnyObject, radius: f32) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    unsafe {
        let _: () = msg_send![v, setWantsLayer: Bool::YES];
        let layer: *mut AnyObject = msg_send![v, layer];
        if layer.is_null() {
            return;
        }
        let _: () = msg_send![layer, setCornerRadius: f64::from(radius)];
        let _: () = msg_send![layer, setMasksToBounds: Bool::YES];
        let curve: *mut AnyObject = msg_send![AnyClass::get(c"NSString").expect("Foundation"), stringWithUTF8String: c"continuous".as_ptr()];
        let _: () = msg_send![layer, setCornerCurve: curve];
    }
}

/// an NSColor for `c`, or nil for none.
#[cfg(target_os = "macos")]
unsafe fn ns_color(c: Option<gpui_kit::Hsla>) -> *mut objc2::runtime::AnyObject {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    let Some(c) = c else { return std::ptr::null_mut() };
    let rgb = gpui_kit::Rgba::from(c);
    unsafe { msg_send![AnyClass::get(c"NSColor").expect("AppKit"), colorWithSRGBRed: f64::from(rgb.r), green: f64::from(rgb.g), blue: f64::from(rgb.b), alpha: f64::from(rgb.a)] }
}

/// the floating glass: tessel's view, then for each pane of glass in turn the
/// glass (bending all that's under it) and a metal layer the renderer draws
/// what's over that glass into.
#[cfg(target_os = "macos")]
fn place_floats(view: usize, floats: &[Spot], dark: bool) {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    let ours = view as *mut AnyObject;
    let Some(glass_class) = AnyClass::get(c"NSGlassEffectView") else { return };
    if ours.is_null() {
        return;
    }
    unsafe {
        let content: *mut AnyObject = msg_send![ours, superview];
        if content.is_null() {
            return;
        }
        let nsstring = AnyClass::get(c"NSString").expect("Foundation");
        let id_of = |kind: &str, i: usize| -> *mut AnyObject {
            let s = std::ffi::CString::new(format!("tessel-glass-{kind}-{i}")).expect("no nul");
            msg_send![nsstring, stringWithUTF8String: s.as_ptr()]
        };
        let find = |id: *mut AnyObject| -> *mut AnyObject {
            let subviews: *mut AnyObject = msg_send![content, subviews];
            let n: usize = msg_send![subviews, count];
            for k in 0..n {
                let v: *mut AnyObject = msg_send![subviews, objectAtIndex: k];
                let vid: *mut AnyObject = msg_send![v, identifier];
                if !vid.is_null() {
                    let same: Bool = msg_send![vid, isEqualToString: id];
                    if same.as_bool() {
                        return v;
                    }
                }
            }
            std::ptr::null_mut()
        };
        let frame: NsRect = msg_send![ours, frame];
        const WIDTH_SIZABLE: usize = 2;
        const HEIGHT_SIZABLE: usize = 16;
        const ABOVE: isize = 1;
        let name: *mut AnyObject = msg_send![nsstring, stringWithUTF8String: if dark { c"NSAppearanceNameDarkAqua".as_ptr() } else { c"NSAppearanceNameAqua".as_ptr() }];
        let look: *mut AnyObject = msg_send![AnyClass::get(c"NSAppearance").expect("AppKit"), appearanceNamed: name];
        // glass that doesn't overlap shares a depth, and a layer over it (the
        // renderer works the depths out the same way, in device pixels).
        let scale = SCALE.with(|s| s.get());
        let rects: Vec<[f64; 4]> = floats.iter().map(|s| [s.x, s.y, s.w, s.h].map(|v| f64::from(v) * scale)).collect();
        let depth = gpui_apple::glass_split::depths(&rects);
        let levels = depth.iter().map(|d| d + 1).max().unwrap_or(0).min(gpui_apple::glass_split::LEVELS);
        let mut below = ours;
        let mut tops = Vec::new();
        for d in 0..gpui_apple::glass_split::LEVELS {
            let (gid, oid) = (id_of("float", d), id_of("over", d));
            let mut holder = find(gid);
            let mut over = find(oid);
            if holder.is_null() && d >= levels {
                break;
            }
            if holder.is_null() {
                let pass = passthrough_class();
                let v: *mut AnyObject = msg_send![pass, alloc];
                let v: *mut AnyObject = msg_send![v, initWithFrame: frame];
                let _: () = msg_send![v, setAutoresizingMask: WIDTH_SIZABLE | HEIGHT_SIZABLE];
                let _: () = msg_send![v, setIdentifier: gid];
                let _: () = msg_send![content, addSubview: v, positioned: ABOVE, relativeTo: below];
                let _: () = msg_send![v, release];
                holder = v;
            }
            if over.is_null() {
                let pass = passthrough_class();
                let v: *mut AnyObject = msg_send![pass, alloc];
                let v: *mut AnyObject = msg_send![v, initWithFrame: frame];
                let _: () = msg_send![v, setAutoresizingMask: WIDTH_SIZABLE | HEIGHT_SIZABLE];
                let _: () = msg_send![v, setIdentifier: oid];
                let _: () = msg_send![v, setWantsLayer: Bool::YES];
                let _: () = msg_send![content, addSubview: v, positioned: ABOVE, relativeTo: holder];
                let _: () = msg_send![v, release];
                let backing: *mut AnyObject = msg_send![v, layer];
                let metal: *mut AnyObject = msg_send![AnyClass::get(c"CAMetalLayer").expect("QuartzCore"), new];
                if backing.is_null() || metal.is_null() {
                    return;
                }
                let bounds: NsRect = msg_send![backing, bounds];
                let _: () = msg_send![metal, setFrame: bounds];
                // kCALayerWidthSizable | kCALayerHeightSizable
                let _: () = msg_send![metal, setAutoresizingMask: 2u32 | 16u32];
                let _: () = msg_send![metal, setOpaque: Bool::NO];
                let _: () = msg_send![backing, addSublayer: metal];
                let _: () = msg_send![metal, release];
                over = v;
            }
            let backing: *mut AnyObject = msg_send![over, layer];
            let sublayers: *mut AnyObject = if backing.is_null() { std::ptr::null_mut() } else { msg_send![backing, sublayers] };
            let metal: *mut AnyObject = if sublayers.is_null() { std::ptr::null_mut() } else { msg_send![sublayers, firstObject] };
            if !metal.is_null() {
                tops.push(metal as usize);
            }
            below = over;
            // this depth's panes of glass: one view each, spares hidden.
            let here: Vec<&Spot> = floats.iter().zip(&depth).filter(|(_, x)| **x == d).map(|(s, _)| s).collect();
            let _: () = msg_send![holder, setHidden: if here.is_empty() { Bool::YES } else { Bool::NO }];
            let bounds: NsRect = msg_send![holder, bounds];
            let mut glasses: Vec<*mut AnyObject> = Vec::new();
            {
                let subs: *mut AnyObject = msg_send![holder, subviews];
                let n: usize = msg_send![subs, count];
                for k in 0..n {
                    glasses.push(msg_send![subs, objectAtIndex: k]);
                }
            }
            while glasses.len() < here.len() {
                let g: *mut AnyObject = msg_send![glass_class, alloc];
                let g: *mut AnyObject = msg_send![g, initWithFrame: NsRect { x: 0., y: 0., w: 1., h: 1. }];
                let _: () = msg_send![holder, addSubview: g];
                let _: () = msg_send![g, release];
                glasses.push(g);
            }
            for (k, g) in glasses.iter().enumerate() {
                let Some(s) = here.get(k) else {
                    let _: () = msg_send![*g, setHidden: Bool::YES];
                    continue;
                };
                let y = bounds.h - f64::from(s.y) - f64::from(s.h);
                let _: () = msg_send![*g, setHidden: Bool::NO];
                let _: () = msg_send![*g, setFrame: NsRect { x: f64::from(s.x), y, w: f64::from(s.w), h: f64::from(s.h) }];
                let _: () = msg_send![*g, setCornerRadius: f64::from(s.radius)];
                round(*g, s.radius);
                let _: () = msg_send![*g, setAppearance: look];
                let _: () = msg_send![*g, setTintColor: ns_color(s.tint)];
            }
        }
        let main: *mut AnyObject = msg_send![ours, layer];
        if !main.is_null() {
            gpui_apple::glass_split::set_top_layers(main as usize, tops);
        }
    }
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
            round(v, s.radius);
            let _: () = msg_send![v, setAppearance: look];
            let _: () = msg_send![v, setTintColor: ns_color(s.tint)];
        }
        for v in have.iter().skip(spots.len()) {
            let _: () = msg_send![*v, removeFromSuperview];
        }
    }
}
