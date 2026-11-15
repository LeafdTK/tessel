//! what's dragged onto the window that gpui doesn't take: gpui accepts files
//! by path, but a screenshot dragged from its thumbnail, or a picture from a
//! browser, comes as a file promised (written once it's dropped) or as an
//! image's bytes. those are taken here, written to a file, and handed to the
//! window as paths, as a dropped file would be. paths still go to gpui.
//!
//! done by wrapping gpui's own window class's drag methods, so whatever is
//! drawn over its view (liquid glass) doesn't matter.

use std::path::PathBuf;
use std::sync::Mutex;

/// where dropped things are handed to the window.
static SINK: Mutex<Option<tokio::sync::mpsc::UnboundedSender<Vec<PathBuf>>>> = Mutex::new(None);

/// take drops for windows from now on, handing their files to `sink`, and
/// let `view`'s window take promised files and pictures too.
pub fn install(view: usize, sink: tokio::sync::mpsc::UnboundedSender<Vec<PathBuf>>) {
    *SINK.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink);
    #[cfg(target_os = "macos")]
    unsafe {
        mac::wrap_classes();
        mac::register(view);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = view;
}

fn hand(paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    if let Some(tx) = SINK.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        let _ = tx.send(paths);
    }
}

/// a fresh folder for one drop's files.
fn drop_dir() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("tessel-drops").join(format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)));
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir)
}

#[cfg(target_os = "macos")]
mod mac {
    use super::{drop_dir, hand};
    use objc2::ffi;
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject, Bool, Imp, Sel};
    use objc2::sel;
    use std::ffi::CStr;
    use std::sync::OnceLock;

    type Entered = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> usize;
    type Perform = unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> Bool;

    /// gpui's own methods, per class, called for drags of paths.
    struct Original {
        entered: Option<Entered>,
        updated: Option<Entered>,
        perform: Option<Perform>,
    }

    static WINDOW: OnceLock<Original> = OnceLock::new();
    static PANEL: OnceLock<Original> = OnceLock::new();

    const COPY: usize = 1;

    fn original(this: &AnyObject) -> Option<&'static Original> {
        let name = this.class().name();
        if name.to_bytes().starts_with(b"GPUIPanel") { PANEL.get() } else { WINDOW.get() }
    }

    pub(super) unsafe fn wrap_classes() {
        for (name, slot) in [(c"GPUIWindow", &WINDOW), (c"GPUIPanel", &PANEL)] {
            if slot.get().is_some() {
                continue;
            }
            let Some(cls) = AnyClass::get(name) else { continue };
            let cls = cls as *const AnyClass as *mut AnyClass;
            unsafe {
                let entered = swap(cls, sel!(draggingEntered:), std::mem::transmute::<Entered, Imp>(entered as Entered));
                let updated = swap(cls, sel!(draggingUpdated:), std::mem::transmute::<Entered, Imp>(updated as Entered));
                let perform = swap(cls, sel!(performDragOperation:), std::mem::transmute::<Perform, Imp>(perform as Perform));
                let _ = slot.set(Original {
                    entered: entered.map(|i| std::mem::transmute::<Imp, Entered>(i)),
                    updated: updated.map(|i| std::mem::transmute::<Imp, Entered>(i)),
                    perform: perform.map(|i| std::mem::transmute::<Imp, Perform>(i)),
                });
            }
        }
    }

    /// put `imp` in `sel`'s place; what was there.
    unsafe fn swap(cls: *mut AnyClass, sel: Sel, imp: Imp) -> Option<Imp> {
        unsafe {
            let method = ffi::class_getInstanceMethod(cls, sel);
            if method.is_null() {
                return None;
            }
            let types = ffi::method_getTypeEncoding(method);
            let old = ffi::method_getImplementation(method);
            ffi::class_replaceMethod(cls, sel, imp, types);
            old
        }
    }

    fn nsstring(s: &CStr) -> *mut AnyObject {
        unsafe { msg_send![AnyClass::get(c"NSString").expect("Foundation"), stringWithUTF8String: s.as_ptr()] }
    }

    /// the window also takes pictures and promised files.
    pub(super) unsafe fn register(view: usize) {
        let view = view as *mut AnyObject;
        if view.is_null() {
            return;
        }
        unsafe {
            let window: *mut AnyObject = msg_send![view, window];
            if window.is_null() {
                return;
            }
            let array: *mut AnyObject = msg_send![AnyClass::get(c"NSMutableArray").expect("Foundation"), array];
            for t in [c"NSFilenamesPboardType", c"public.file-url", c"public.png", c"public.tiff", c"public.jpeg"] {
                let _: () = msg_send![array, addObject: nsstring(t)];
            }
            if let Some(receiver) = AnyClass::get(c"NSFilePromiseReceiver") {
                let promised: *mut AnyObject = msg_send![receiver, readableDraggedTypes];
                if !promised.is_null() {
                    let _: () = msg_send![array, addObjectsFromArray: promised];
                }
            }
            let _: () = msg_send![window, registerForDraggedTypes: array];
        }
    }

    /// what a drag carries, by the kinds on its pasteboard.
    #[derive(PartialEq)]
    enum Carries {
        Paths,
        Promise,
        Picture,
        Nothing,
    }

    unsafe fn carries(info: *mut AnyObject) -> Carries {
        unsafe {
            let pb: *mut AnyObject = msg_send![info, draggingPasteboard];
            if pb.is_null() {
                return Carries::Nothing;
            }
            let types: *mut AnyObject = msg_send![pb, types];
            if types.is_null() {
                return Carries::Nothing;
            }
            let has = |t: &CStr| -> bool {
                let b: Bool = msg_send![types, containsObject: nsstring(t)];
                b.as_bool()
            };
            if has(c"NSFilenamesPboardType") {
                return Carries::Paths;
            }
            if has(c"com.apple.NSFilePromiseItemMetaData") || has(c"com.apple.pasteboard.promised-file-url") || has(c"NSPromiseContentsPboardType") {
                return Carries::Promise;
            }
            if has(c"public.png") || has(c"public.tiff") || has(c"public.jpeg") {
                return Carries::Picture;
            }
            Carries::Nothing
        }
    }

    unsafe extern "C-unwind" fn entered(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> usize {
        unsafe {
            match carries(info) {
                Carries::Promise | Carries::Picture => COPY,
                _ => original(this).and_then(|o| o.entered).map_or(0, |f| f(this, sel, info)),
            }
        }
    }

    unsafe extern "C-unwind" fn updated(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> usize {
        unsafe {
            match carries(info) {
                Carries::Promise | Carries::Picture => COPY,
                _ => original(this).and_then(|o| o.updated).map_or(0, |f| f(this, sel, info)),
            }
        }
    }

    unsafe extern "C-unwind" fn perform(this: &AnyObject, sel: Sel, info: *mut AnyObject) -> Bool {
        unsafe {
            match carries(info) {
                Carries::Promise => Bool::new(take_promises(info)),
                Carries::Picture => Bool::new(take_picture(info)),
                _ => original(this).and_then(|o| o.perform).map_or(Bool::NO, |f| f(this, sel, info)),
            }
        }
    }

    /// promised files: written into a folder of their own, each handed over as it lands.
    unsafe fn take_promises(info: *mut AnyObject) -> bool {
        unsafe {
            let Some(receiver) = AnyClass::get(c"NSFilePromiseReceiver") else { return false };
            let pb: *mut AnyObject = msg_send![info, draggingPasteboard];
            let classes: *mut AnyObject = msg_send![AnyClass::get(c"NSArray").expect("Foundation"), arrayWithObject: receiver as *const AnyClass as *mut AnyObject];
            let options: *mut AnyObject = msg_send![AnyClass::get(c"NSDictionary").expect("Foundation"), dictionary];
            let promises: *mut AnyObject = msg_send![pb, readObjectsForClasses: classes, options: options];
            if promises.is_null() {
                return false;
            }
            let n: usize = msg_send![promises, count];
            let Some(dir) = drop_dir() else { return false };
            let dir_s = std::ffi::CString::new(dir.to_string_lossy().into_owned()).unwrap_or_default();
            let url: *mut AnyObject = msg_send![AnyClass::get(c"NSURL").expect("Foundation"), fileURLWithPath: nsstring(&dir_s)];
            let queue: Retained<AnyObject> = msg_send![AnyClass::get(c"NSOperationQueue").expect("Foundation"), new];
            for i in 0..n {
                let promise: *mut AnyObject = msg_send![promises, objectAtIndex: i];
                let reader = block2::RcBlock::new(|file: *mut AnyObject, error: *mut AnyObject| {
                    if !error.is_null() || file.is_null() {
                        return;
                    }
                    let path: *mut AnyObject = msg_send![file, path];
                    if path.is_null() {
                        return;
                    }
                    let c: *const std::ffi::c_char = msg_send![path, UTF8String];
                    if !c.is_null() {
                        hand(vec![std::path::PathBuf::from(CStr::from_ptr(c).to_string_lossy().into_owned())]);
                    }
                });
                let _: () = msg_send![promise, receivePromisedFilesAtDestination: url, options: options, operationQueue: &*queue, reader: &*reader];
            }
            n > 0
        }
    }

    /// a picture's bytes: kept as a png.
    unsafe fn take_picture(info: *mut AnyObject) -> bool {
        unsafe {
            let pb: *mut AnyObject = msg_send![info, draggingPasteboard];
            let png: *mut AnyObject = msg_send![pb, dataForType: nsstring(c"public.png")];
            let data = if !png.is_null() {
                png
            } else {
                // tiff (or jpeg) as it came: made a png.
                let mut raw: *mut AnyObject = msg_send![pb, dataForType: nsstring(c"public.tiff")];
                if raw.is_null() {
                    raw = msg_send![pb, dataForType: nsstring(c"public.jpeg")];
                }
                if raw.is_null() {
                    return false;
                }
                let rep: *mut AnyObject = msg_send![AnyClass::get(c"NSBitmapImageRep").expect("AppKit"), imageRepWithData: raw];
                if rep.is_null() {
                    return false;
                }
                let props: *mut AnyObject = msg_send![AnyClass::get(c"NSDictionary").expect("Foundation"), dictionary];
                // NSBitmapImageFileTypePNG
                msg_send![rep, representationUsingType: 4usize, properties: props]
            };
            if data.is_null() {
                return false;
            }
            let len: usize = msg_send![data, length];
            let bytes: *const std::ffi::c_void = msg_send![data, bytes];
            let bytes = bytes.cast::<u8>();
            if bytes.is_null() || len == 0 {
                return false;
            }
            let Some(dir) = drop_dir() else { return false };
            let path = dir.join("Picture.png");
            if std::fs::write(&path, std::slice::from_raw_parts(bytes, len)).is_err() {
                return false;
            }
            hand(vec![path]);
            true
        }
    }
}
