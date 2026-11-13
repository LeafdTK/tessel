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
