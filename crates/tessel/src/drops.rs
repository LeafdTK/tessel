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

/// a file dragged out of the window: one on this mac (its path), or one on a
/// dev box, written where it's dropped once it is (fetched by `fetch`).
pub enum Outgoing {
    Here(PathBuf),
    There { name: String, fetch: Box<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync> },
}

/// start dragging `what` out of `view`'s window, from the mouse event being
/// handled now. false if it couldn't start.
pub fn drag_out(view: usize, what: Outgoing) -> bool {
    #[cfg(target_os = "macos")]
    unsafe {
        out::start(view, what)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (view, what);
        false
    }
}

#[cfg(target_os = "macos")]
mod out {
    use super::Outgoing;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, ClassBuilder, Sel};
    use objc2::sel;
    use std::ffi::c_void;
    use std::sync::OnceLock;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Point {
        x: f64,
        y: f64,
    }
    unsafe impl objc2::encode::Encode for Point {
        const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }
    unsafe impl objc2::encode::Encode for Rect {
        const ENCODING: objc2::encode::Encoding = {
            use objc2::encode::Encoding;
            Encoding::Struct("CGRect", &[Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]), Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING])])
        };
    }

    fn nsstring(s: &str) -> *mut AnyObject {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        unsafe { msg_send![AnyClass::get(c"NSString").expect("Foundation"), stringWithUTF8String: c.as_ptr()] }
    }

    /// what a promise is of: kept alive as long as its delegate is (leaked: a drag at a time).
    struct Promise {
        name: String,
        fetch: Box<dyn Fn() -> Result<Vec<u8>, String> + Send + Sync>,
    }

    extern "C-unwind" fn op_mask(_: &AnyObject, _: Sel, _: *mut AnyObject, _: isize) -> usize {
        1 // copy
    }

    extern "C-unwind" fn file_name(this: &AnyObject, _: Sel, _: *mut AnyObject, _: *mut AnyObject) -> *mut AnyObject {
        let p = unsafe { promise_of(this) };
        nsstring(p.map_or("file", |p| p.name.as_str()))
    }

    extern "C-unwind" fn queue(_: &AnyObject, _: Sel, _: *mut AnyObject) -> *mut AnyObject {
        // written off the main thread: fetching it waits on the window, which is the main thread.
        static Q: OnceLock<usize> = OnceLock::new();
        *Q.get_or_init(|| {
            let q: *mut AnyObject = unsafe { msg_send![AnyClass::get(c"NSOperationQueue").expect("Foundation"), new] };
            q as usize
        }) as *mut AnyObject
    }

    extern "C-unwind" fn write(this: &AnyObject, _: Sel, _: *mut AnyObject, url: *mut AnyObject, done: *mut c_void) {
        let block = done as *const block2::Block<dyn Fn(*mut AnyObject)>;
        let result = unsafe { promise_of(this) }.ok_or_else(|| "it went".to_owned()).and_then(|p| (p.fetch)()).and_then(|bytes| {
            let path: *mut AnyObject = unsafe { msg_send![url, path] };
            let c: *const std::ffi::c_char = unsafe { msg_send![path, UTF8String] };
            let path = unsafe { std::ffi::CStr::from_ptr(c) }.to_string_lossy().into_owned();
            std::fs::write(&path, bytes).map_err(|e| e.to_string())
        });
        let error: *mut AnyObject = match result {
            Ok(()) => std::ptr::null_mut(),
            Err(e) => unsafe {
                let info: *mut AnyObject = msg_send![AnyClass::get(c"NSDictionary").expect("Foundation"), dictionaryWithObject: nsstring(&e), forKey: nsstring("NSLocalizedDescription")];
                msg_send![AnyClass::get(c"NSError").expect("Foundation"), errorWithDomain: nsstring("tessel"), code: 1isize, userInfo: info]
            },
        };
        if !block.is_null() {
            unsafe { (*block).call((error,)) };
        }
    }

    unsafe fn promise_of(this: &AnyObject) -> Option<&'static Promise> {
        let ivar = this.class().instance_variable(c"promise")?;
        let p: *mut c_void = unsafe { *ivar.load::<*mut c_void>(this) };
        (!p.is_null()).then(|| unsafe { &*(p as *const Promise) })
    }

    fn classes() -> (&'static AnyClass, &'static AnyClass) {
        static C: OnceLock<(usize, usize)> = OnceLock::new();
        let (s, d) = *C.get_or_init(|| unsafe {
            let source = AnyClass::get(c"TesselDragSource").map(|c| c as *const AnyClass as usize).unwrap_or_else(|| {
                let mut b = ClassBuilder::new(c"TesselDragSource", AnyClass::get(c"NSObject").expect("Foundation")).expect("a class");
                b.add_method(sel!(draggingSession:sourceOperationMaskForDraggingContext:), op_mask as extern "C-unwind" fn(_, _, _, _) -> _);
                b.register() as *const AnyClass as usize
            });
            let delegate = AnyClass::get(c"TesselPromiseDelegate").map(|c| c as *const AnyClass as usize).unwrap_or_else(|| {
                let mut b = ClassBuilder::new(c"TesselPromiseDelegate", AnyClass::get(c"NSObject").expect("Foundation")).expect("a class");
                b.add_ivar::<*mut c_void>(c"promise");
                b.add_method(sel!(filePromiseProvider:fileNameForType:), file_name as extern "C-unwind" fn(_, _, _, _) -> _);
                b.add_method(sel!(operationQueueForFilePromiseProvider:), queue as extern "C-unwind" fn(_, _, _) -> _);
                b.add_method(sel!(filePromiseProvider:writePromiseToURL:completionHandler:), write as extern "C-unwind" fn(_, _, _, _, _));
                b.register() as *const AnyClass as usize
            });
            (source, delegate)
        });
        unsafe { (&*(s as *const AnyClass), &*(d as *const AnyClass)) }
    }

    pub(super) unsafe fn start(view: usize, what: Outgoing) -> bool {
        let view = view as *mut AnyObject;
        if view.is_null() {
            return false;
        }
        unsafe {
            let app: *mut AnyObject = msg_send![AnyClass::get(c"NSApplication").expect("AppKit"), sharedApplication];
            let event: *mut AnyObject = msg_send![app, currentEvent];
            if event.is_null() {
                return false;
            }
            let (source_class, delegate_class) = classes();
            let (writer, ext): (*mut AnyObject, String) = match what {
                Outgoing::Here(path) => {
                    let url: *mut AnyObject = msg_send![AnyClass::get(c"NSURL").expect("Foundation"), fileURLWithPath: nsstring(&path.to_string_lossy())];
                    (url, path.extension().map(|e| e.to_string_lossy().into_owned()).unwrap_or_default())
                }
                Outgoing::There { name, fetch } => {
                    let Some(provider_class) = AnyClass::get(c"NSFilePromiseProvider") else { return false };
                    let delegate: *mut AnyObject = msg_send![delegate_class, new];
                    let ext = name.rsplit_once('.').map(|(_, e)| e.to_owned()).unwrap_or_default();
                    let promise = Box::into_raw(Box::new(Promise { name, fetch })) as *mut c_void;
                    if let Some(ivar) = delegate_class.instance_variable(c"promise") {
                        *ivar.load_mut::<*mut c_void>(&mut *delegate) = promise;
                    }
                    let provider: *mut AnyObject = msg_send![provider_class, alloc];
                    let provider: *mut AnyObject = msg_send![provider, initWithFileType: nsstring("public.data"), delegate: delegate];
                    (provider, ext)
                }
            };
            let item: *mut AnyObject = msg_send![AnyClass::get(c"NSDraggingItem").expect("AppKit"), alloc];
            let item: *mut AnyObject = msg_send![item, initWithPasteboardWriter: writer];
            let ws: *mut AnyObject = msg_send![AnyClass::get(c"NSWorkspace").expect("AppKit"), sharedWorkspace];
            let icon: *mut AnyObject = msg_send![ws, iconForFileType: nsstring(if ext.is_empty() { "public.data" } else { &ext })];
            let at: Point = msg_send![event, locationInWindow];
            let at: Point = msg_send![view, convertPoint: at, fromView: std::ptr::null_mut::<AnyObject>()];
            let _: () = msg_send![item, setDraggingFrame: Rect { x: at.x - 16.0, y: at.y - 16.0, w: 32.0, h: 32.0 }, contents: icon];
            let items: *mut AnyObject = msg_send![AnyClass::get(c"NSArray").expect("Foundation"), arrayWithObject: item];
            let source: *mut AnyObject = msg_send![source_class, new];
            let session: *mut AnyObject = msg_send![view, beginDraggingSessionWithItems: items, event: event, source: source];
            !session.is_null()
        }
    }
}

/// what's on the clipboard to attach, as files: copied files (from the
/// Finder), or a copied picture (written to a png first). none when it's text
/// to paste as text. which one the copying app offered first wins: a browser's
/// "Copy Image" leads with the picture (its address trails), a spreadsheet's cells
/// lead with their text (a picture of them trails). tests never read the real one.
pub fn clipboard_files() -> Option<Vec<PathBuf>> {
    #[cfg(all(target_os = "macos", not(test)))]
    unsafe {
        clip::files()
    }
    #[cfg(any(not(target_os = "macos"), test))]
    None
}

#[cfg(target_os = "macos")]
mod clip {
    use super::drop_dir;
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ffi::CStr;
    use std::path::PathBuf;

    fn nsstring(s: &CStr) -> *mut AnyObject {
        unsafe { msg_send![AnyClass::get(c"NSString").expect("Foundation"), stringWithUTF8String: s.as_ptr()] }
    }

    unsafe fn text_of(o: *mut AnyObject) -> Option<String> {
        if o.is_null() {
            return None;
        }
        let c: *const std::ffi::c_char = unsafe { msg_send![o, UTF8String] };
        (!c.is_null()).then(|| unsafe { CStr::from_ptr(c) }.to_string_lossy().into_owned())
    }

    #[cfg_attr(test, allow(dead_code))]
    pub(super) unsafe fn files() -> Option<Vec<PathBuf>> {
        unsafe { files_on(msg_send![AnyClass::get(c"NSPasteboard").expect("AppKit"), generalPasteboard]) }
    }

    pub(super) unsafe fn files_on(pb: *mut AnyObject) -> Option<Vec<PathBuf>> {
        unsafe {
            let types: *mut AnyObject = msg_send![pb, types];
            if types.is_null() {
                return None;
            }
            let has = |t: &CStr| -> bool {
                let b: Bool = msg_send![types, containsObject: nsstring(t)];
                b.as_bool()
            };
            let place = |t: &CStr| -> usize { msg_send![types, indexOfObject: nsstring(t)] };
            // files copied in the Finder.
            if has(c"NSFilenamesPboardType") || has(c"public.file-url") {
                let yes: *mut AnyObject = msg_send![AnyClass::get(c"NSNumber").expect("Foundation"), numberWithBool: Bool::YES];
                let classes: *mut AnyObject = msg_send![AnyClass::get(c"NSArray").expect("Foundation"), arrayWithObject: AnyClass::get(c"NSURL").expect("Foundation") as *const AnyClass as *mut AnyObject];
                let only_files: *mut AnyObject = msg_send![AnyClass::get(c"NSDictionary").expect("Foundation"), dictionaryWithObject: yes, forKey: nsstring(c"NSPasteboardURLReadingFileURLsOnlyKey")];
                let urls: *mut AnyObject = msg_send![pb, readObjectsForClasses: classes, options: only_files];
                if !urls.is_null() {
                    let n: usize = msg_send![urls, count];
                    let paths: Vec<PathBuf> = (0..n)
                        .filter_map(|i| {
                            let u: *mut AnyObject = msg_send![urls, objectAtIndex: i];
                            text_of(msg_send![u, path]).map(PathBuf::from)
                        })
                        .collect();
                    if !paths.is_empty() {
                        return Some(paths);
                    }
                }
            }
            let picture = [c"public.png", c"public.tiff", c"public.jpeg"].into_iter().filter(|t| has(t)).min_by_key(|t| place(t))?;
            // NSNotFound is usize::MAX, so a clipboard without text never beats the picture.
            let text = [c"public.utf8-plain-text", c"NSStringPboardType", c"public.rtf"].into_iter().map(|t| place(t)).min().unwrap_or(usize::MAX);
            if text < place(picture) {
                return None;
            }
            let data: *mut AnyObject = msg_send![pb, dataForType: nsstring(picture)];
            if data.is_null() {
                return None;
            }
            let png = if picture == c"public.png" {
                data
            } else {
                let rep: *mut AnyObject = msg_send![AnyClass::get(c"NSBitmapImageRep").expect("AppKit"), imageRepWithData: data];
                if rep.is_null() {
                    return None;
                }
                let props: *mut AnyObject = msg_send![AnyClass::get(c"NSDictionary").expect("Foundation"), dictionary];
                msg_send![rep, representationUsingType: 4usize, properties: props]
            };
            if png.is_null() {
                return None;
            }
            let len: usize = msg_send![png, length];
            let bytes: *const std::ffi::c_void = msg_send![png, bytes];
            if bytes.is_null() || len == 0 {
                return None;
            }
            let dir = drop_dir()?;
            let path = dir.join("Pasted image.png");
            std::fs::write(&path, std::slice::from_raw_parts(bytes.cast::<u8>(), len)).ok()?;
            Some(vec![path])
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod clip_tests {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use std::ffi::CStr;

    fn ns(s: &CStr) -> *mut AnyObject {
        unsafe { msg_send![AnyClass::get(c"NSString").unwrap(), stringWithUTF8String: s.as_ptr()] }
    }

    /// a private pasteboard holding `items` (type, bytes), in that order.
    fn board(items: &[(&CStr, &[u8])]) -> *mut AnyObject {
        unsafe {
            let pb: *mut AnyObject = msg_send![AnyClass::get(c"NSPasteboard").unwrap(), pasteboardWithUniqueName];
            let types: *mut AnyObject = msg_send![AnyClass::get(c"NSMutableArray").unwrap(), array];
            for (t, _) in items {
                let _: () = msg_send![types, addObject: ns(t)];
            }
            let _: isize = msg_send![pb, declareTypes: types, owner: std::ptr::null_mut::<AnyObject>()];
            for (t, bytes) in items {
                let data: *mut AnyObject = msg_send![AnyClass::get(c"NSData").unwrap(), dataWithBytes: bytes.as_ptr().cast::<std::ffi::c_void>(), length: bytes.len()];
                let ok: Bool = msg_send![pb, setData: data, forType: ns(t)];
                assert!(ok.as_bool());
            }
            pb
        }
    }

    fn png() -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255])).write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_copied_picture_pastes_as_a_png_and_copied_text_stays_text() {
        let pic = png();
        // a browser's copy image: the picture first, its address after.
        let got = unsafe { super::clip::files_on(board(&[(c"public.png", &pic), (c"public.utf8-plain-text", b"https://x.test/a.png")])) }.expect("the picture");
        assert_eq!(std::fs::read(&got[0]).unwrap(), pic);
        // a screenshot is only a tiff: it comes out a png.
        let mut tiff = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(3, 2, image::Rgba([0, 0, 255, 255])).write_to(&mut tiff, image::ImageFormat::Tiff).unwrap();
        let got = unsafe { super::clip::files_on(board(&[(c"public.tiff", tiff.get_ref())])) }.expect("the screenshot");
        let back = image::open(&got[0]).unwrap();
        assert_eq!((back.width(), back.height()), (3, 2));
        assert!(std::fs::read(&got[0]).unwrap().starts_with(b"\x89PNG"));
        // cells copied from a spreadsheet: text first, a picture of them after.
        assert!(unsafe { super::clip::files_on(board(&[(c"public.utf8-plain-text", b"42"), (c"public.png", &pic)])) }.is_none());
        assert!(unsafe { super::clip::files_on(board(&[(c"public.utf8-plain-text", b"hi")])) }.is_none());
        // files copied in the finder.
        let dir = std::env::temp_dir().join(format!("tessel-clip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("notes.txt");
        std::fs::write(&f, "x").unwrap();
        let url = format!("file://{}", f.display());
        let got = unsafe { super::clip::files_on(board(&[(c"public.file-url", url.as_bytes()), (c"public.utf8-plain-text", b"notes.txt")])) }.expect("the file");
        assert_eq!(got, vec![f]);
    }
}
