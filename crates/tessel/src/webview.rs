//! pages in the window itself: a web view of the system's own (WebKit, on the
//! Mac) for each chat that has a browser, put where the Browser pane is. the
//! system draws it as it draws any browser's page, at the screen's resolution
//! and refresh rate, and it takes the pointer, scrolling and keys directly.
//!
//! it's a view of its own over the window's content, put where the pane is
//! each frame, and stepped out of the way while a menu or a sheet is open
//! over it (gpui draws those in its own view, under this one).
//!
//! what an agent does to the page (relayed by the daemon) is done here, with
//! the scripts at the bottom: they find things, click, type and scroll the way
//! a person's input would, and let the person point at an element.

use tokio::sync::oneshot;

pub use imp::{Awake, WebView, reclaim_focus};

/// what a script answered: its value, already parsed from JSON.
pub type Answer = oneshot::Receiver<Result<serde_json::Value, String>>;
/// a picture of the page, PNG.
pub type Picture = oneshot::Receiver<Result<Vec<u8>, String>>;

#[cfg(target_os = "macos")]
mod imp {
    use super::{Answer, Picture};
    use objc2::encode::{Encode, Encoding};
    use objc2::rc::{Allocated, Retained};
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2::{msg_send, sel};
    use std::cell::Cell;
    use std::ffi::CStr;
    use tokio::sync::oneshot;

    #[link(name = "WebKit", kind = "framework")]
    unsafe extern "C" {}

    #[repr(C)]
    #[derive(Clone, Copy, Debug, Default)]
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

    fn class(name: &CStr) -> Option<&'static AnyClass> {
        AnyClass::get(name)
    }

    fn ns_string(s: &str) -> Retained<AnyObject> {
        unsafe {
            let a: Allocated<AnyObject> = msg_send![class(c"NSString").expect("Foundation"), alloc];
            const UTF8: usize = 4;
            let s: Option<Retained<AnyObject>> = msg_send![a, initWithBytes: s.as_ptr().cast::<std::ffi::c_void>(), length: s.len(), encoding: UTF8];
            s.expect("a string")
        }
    }

    /// an NSString's text; empty for nil.
    fn text_of(s: *mut AnyObject) -> String {
        if s.is_null() {
            return String::new();
        }
        unsafe {
            let p: *const std::ffi::c_char = msg_send![s, UTF8String];
            if p.is_null() { String::new() } else { CStr::from_ptr(p).to_string_lossy().into_owned() }
        }
    }

    fn error_text(e: *mut AnyObject) -> String {
        if e.is_null() {
            return "the page didn't answer".into();
        }
        text_of(unsafe { msg_send![e, localizedDescription] })
    }

    /// one page.
    pub struct WebView {
        view: Retained<AnyObject>,
    }

    impl WebView {
        /// a page in `parent` (the window's view), out of sight until placed.
        pub fn new(parent: usize) -> Option<WebView> {
            let container = parent as *mut AnyObject;
            if container.is_null() {
                return None;
            }
            unsafe {
                let config: Retained<AnyObject> = msg_send![class(c"WKWebViewConfiguration")?, new];
                // sites see Safari, which is what this is.
                let _: () = msg_send![&*config, setApplicationNameForUserAgent: &*ns_string("Version/18.0 Safari/605.1.15")];
                let frame = Rect { x: 0., y: 0., w: 1280., h: 800. };
                let a: Allocated<AnyObject> = msg_send![class(c"WKWebView")?, alloc];
                let view: Option<Retained<AnyObject>> = msg_send![a, initWithFrame: frame, configuration: &*config];
                let view = view?;
                let _: () = msg_send![&*view, setHidden: Bool::YES];
                let _: () = msg_send![&*view, setAllowsBackForwardNavigationGestures: Bool::YES];
                let _: () = msg_send![&*view, setAllowsMagnification: Bool::YES];
                // Web Inspector, from the page's context menu.
                let inspectable: Bool = msg_send![&*view, respondsToSelector: sel!(setInspectable:)];
                if inspectable.as_bool() {
                    let _: () = msg_send![&*view, setInspectable: Bool::YES];
                }
                let _: () = msg_send![container, addSubview: &*view];
                Some(WebView { view })
            }
        }

        /// where it is, in points from the parent's top left, and how round
        /// its bottom corners are (the pane's, so the page fits it).
        pub fn place(&self, x: f64, y: f64, w: f64, h: f64, radius: f64) {
            unsafe {
                let layer: *mut AnyObject = msg_send![&*self.view, layer];
                if !layer.is_null() {
                    let now: f64 = msg_send![layer, cornerRadius];
                    if now != radius {
                        let _: () = msg_send![layer, setCornerRadius: radius];
                        let _: () = msg_send![layer, setMasksToBounds: Bool::new(radius > 0.0)];
                        // the corners at the bottom of the screen, whichever way up the layer is.
                        let flipped: Bool = msg_send![layer, isGeometryFlipped];
                        const MIN_Y: usize = 1 | 2;
                        const MAX_Y: usize = 4 | 8;
                        let _: () = msg_send![layer, setMaskedCorners: if flipped.as_bool() { MAX_Y } else { MIN_Y }];
                    }
                }
                let parent: *mut AnyObject = msg_send![&*self.view, superview];
                if parent.is_null() {
                    return;
                }
                let flipped: Bool = msg_send![parent, isFlipped];
                let pb: Rect = msg_send![parent, bounds];
                let y = if flipped.as_bool() { y } else { pb.h - y - h };
                let next = Rect { x, y, w, h };
                let now: Rect = msg_send![&*self.view, frame];
                if (now.x, now.y, now.w, now.h) != (next.x, next.y, next.w, next.h) {
                    let _: () = msg_send![&*self.view, setFrame: next];
                }
            }
        }

        pub fn show(&self, on: bool) {
            unsafe {
                let hidden: Bool = msg_send![&*self.view, isHidden];
                if hidden.as_bool() == on {
                    let _: () = msg_send![&*self.view, setHidden: Bool::new(!on)];
                }
            }
        }

        pub fn load(&self, url: &str) -> Result<(), String> {
            unsafe {
                let u: *mut AnyObject = msg_send![class(c"NSURL").ok_or("no Foundation")?, URLWithString: &*ns_string(url)];
                if u.is_null() {
                    return Err(format!("{url} isn't an address"));
                }
                let req: *mut AnyObject = msg_send![class(c"NSURLRequest").ok_or("no Foundation")?, requestWithURL: u];
                let _: *mut AnyObject = msg_send![&*self.view, loadRequest: req];
            }
            Ok(())
        }

        pub fn back(&self) {
            let _: *mut AnyObject = unsafe { msg_send![&*self.view, goBack] };
        }
        pub fn forward(&self) {
            let _: *mut AnyObject = unsafe { msg_send![&*self.view, goForward] };
        }
        pub fn reload(&self) {
            let _: *mut AnyObject = unsafe { msg_send![&*self.view, reload] };
        }

        pub fn url(&self) -> String {
            unsafe {
                let u: *mut AnyObject = msg_send![&*self.view, URL];
                if u.is_null() {
                    return String::new();
                }
                text_of(msg_send![u, absoluteString])
            }
        }
        pub fn title(&self) -> String {
            text_of(unsafe { msg_send![&*self.view, title] })
        }
        pub fn loading(&self) -> bool {
            let b: Bool = unsafe { msg_send![&*self.view, isLoading] };
            b.as_bool()
        }

        /// run `js`, an expression, in the page; its value, through JSON.
        pub fn eval(&self, js: &str) -> Answer {
            let (tx, rx) = oneshot::channel();
            let wrapped = format!("(function(){{try{{return JSON.stringify({{ok:({js})}})}}catch(e){{return JSON.stringify({{err:String(e&&e.message||e)}})}}}})()");
            let tx = Cell::new(Some(tx));
            let done = block2::RcBlock::new(move |result: *mut AnyObject, error: *mut AnyObject| {
                let Some(tx) = tx.take() else { return };
                let answer = if result.is_null() {
                    Err(error_text(error))
                } else {
                    serde_json::from_str::<serde_json::Value>(&text_of(result)).map_err(|e| e.to_string()).and_then(|v| match v.get("err").and_then(|e| e.as_str()) {
                        Some(e) => Err(e.to_owned()),
                        None => Ok(v.get("ok").cloned().unwrap_or(serde_json::Value::Null)),
                    })
                };
                let _ = tx.send(answer);
            });
            unsafe {
                let _: () = msg_send![&*self.view, evaluateJavaScript: &*ns_string(&wrapped), completionHandler: &*done];
            }
            rx
        }

        /// a picture of the page as it is on screen, or of `rect` of it (in the
        /// page's points, from its top left).
        pub fn snapshot(&self, rect: Option<(f64, f64, f64, f64)>) -> Picture {
            let (tx, rx) = oneshot::channel();
            let tx = Cell::new(Some(tx));
            let done = block2::RcBlock::new(move |image: *mut AnyObject, error: *mut AnyObject| {
                let Some(tx) = tx.take() else { return };
                let _ = tx.send(if image.is_null() { Err(error_text(error)) } else { png_of(image) });
            });
            unsafe {
                let Some(cls) = class(c"WKSnapshotConfiguration") else {
                    let _ = done.call((std::ptr::null_mut(), std::ptr::null_mut()));
                    return rx;
                };
                let config: Retained<AnyObject> = msg_send![cls, new];
                if let Some((x, y, w, h)) = rect {
                    let _: () = msg_send![&*config, setRect: Rect { x, y, w, h }];
                }
                // what's drawn now: waiting for the next screen update never ends
                // while the window is covered, and holds up the page's scripts too.
                let _: () = msg_send![&*config, setAfterScreenUpdates: Bool::NO];
                let _: () = msg_send![&*self.view, takeSnapshotWithConfiguration: &*config, completionHandler: &*done];
            }
            rx
        }

        /// whether the keyboard is the page's now.
        pub fn has_focus(&self) -> bool {
            unsafe {
                let win: *mut AnyObject = msg_send![&*self.view, window];
                if win.is_null() {
                    return false;
                }
                let first: *mut AnyObject = msg_send![win, firstResponder];
                is_inside(first, &self.view)
            }
        }
    }

    impl Drop for WebView {
        fn drop(&mut self) {
            unsafe {
                let _: () = msg_send![&*self.view, stopLoading];
                let _: () = msg_send![&*self.view, removeFromSuperview];
            }
        }
    }

    /// is `responder` the view, or inside it.
    unsafe fn is_inside(responder: *mut AnyObject, view: &AnyObject) -> bool {
        if responder.is_null() {
            return false;
        }
        unsafe {
            let is_view: Bool = msg_send![responder, isKindOfClass: class(c"NSView").expect("AppKit")];
            if !is_view.as_bool() {
                return false;
            }
            let inside: Bool = msg_send![responder, isDescendantOf: view];
            inside.as_bool()
        }
    }

    fn png_of(image: *mut AnyObject) -> Result<Vec<u8>, String> {
        unsafe {
            let tiff: *mut AnyObject = msg_send![image, TIFFRepresentation];
            let rep: *mut AnyObject = msg_send![class(c"NSBitmapImageRep").ok_or("no AppKit")?, imageRepWithData: tiff];
            if rep.is_null() {
                return Err("the picture couldn't be read".into());
            }
            let props: *mut AnyObject = msg_send![class(c"NSDictionary").ok_or("no Foundation")?, dictionary];
            const PNG: usize = 4;
            let png: *mut AnyObject = msg_send![rep, representationUsingType: PNG, properties: props];
            if png.is_null() {
                return Err("the picture couldn't be made a PNG".into());
            }
            let len: usize = msg_send![png, length];
            let bytes: *const std::ffi::c_void = msg_send![png, bytes];
            Ok(if bytes.is_null() { Vec::new() } else { std::slice::from_raw_parts(bytes.cast::<u8>(), len).to_vec() })
        }
    }

    /// while it's held, macOS doesn't slow the app down for being in the
    /// background: an agent driving a page shouldn't wait on App Nap.
    pub struct Awake(Option<Retained<AnyObject>>);

    impl Awake {
        pub fn new(reason: &str) -> Awake {
            unsafe {
                let Some(cls) = class(c"NSProcessInfo") else { return Awake(None) };
                let info: *mut AnyObject = msg_send![cls, processInfo];
                // user initiated, letting the Mac sleep when idle.
                const OPTIONS: u64 = 0x00FF_FFFF & !(1 << 20);
                let token: Option<Retained<AnyObject>> = msg_send![info, beginActivityWithOptions: OPTIONS, reason: &*ns_string(reason)];
                Awake(token)
            }
        }
    }

    impl Drop for Awake {
        fn drop(&mut self) {
            if let Some(token) = self.0.take() {
                unsafe {
                    let info: *mut AnyObject = msg_send![class(c"NSProcessInfo").expect("Foundation"), processInfo];
                    let _: () = msg_send![info, endActivity: &*token];
                }
            }
        }
    }

    /// the keyboard back to the window's own content, when a page has it and
    /// the person clicked elsewhere. `gpui_view`: the window's view.
    pub fn reclaim_focus(gpui_view: usize) {
        let view = gpui_view as *mut AnyObject;
        if view.is_null() {
            return;
        }
        unsafe {
            let win: *mut AnyObject = msg_send![view, window];
            if win.is_null() {
                return;
            }
            let first: *mut AnyObject = msg_send![win, firstResponder];
            let Some(web) = class(c"WKWebView") else { return };
            // only a page's focus is taken back.
            let mut v = first;
            let mut in_page = false;
            while !v.is_null() {
                let is_view: Bool = msg_send![v, isKindOfClass: class(c"NSView").expect("AppKit")];
                if !is_view.as_bool() {
                    break;
                }
                let is_web: Bool = msg_send![v, isKindOfClass: web];
                if is_web.as_bool() {
                    in_page = true;
                    break;
                }
                v = msg_send![v, superview];
            }
            if !in_page {
                return;
            }
            let took: Bool = msg_send![win, makeFirstResponder: view];
            if !took.as_bool() {
                let _: Bool = msg_send![win, makeFirstResponder: std::ptr::null_mut::<AnyObject>()];
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::{Answer, Picture};
    use tokio::sync::oneshot;

    /// no native pages here: the daemon's browser is used instead.
    pub struct WebView;

    impl WebView {
        pub fn new(_: usize) -> Option<WebView> {
            None
        }
        pub fn load(&self, _: &str) -> Result<(), String> {
            Err("no pages here".into())
        }
        pub fn place(&self, _: f64, _: f64, _: f64, _: f64, _: f64) {}
        pub fn show(&self, _: bool) {}
        pub fn back(&self) {}
        pub fn forward(&self) {}
        pub fn reload(&self) {}
        pub fn url(&self) -> String {
            String::new()
        }
        pub fn title(&self) -> String {
            String::new()
        }
        pub fn loading(&self) -> bool {
            false
        }
        pub fn eval(&self, _: &str) -> Answer {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Err("no pages here".into()));
            rx
        }
        pub fn snapshot(&self, _: Option<(f64, f64, f64, f64)>) -> Picture {
            let (tx, rx) = oneshot::channel();
            let _ = tx.send(Err("no pages here".into()));
            rx
        }
        pub fn has_focus(&self) -> bool {
            false
        }
    }

    pub fn reclaim_focus(_: usize) {}

    pub struct Awake;
    impl Awake {
        pub fn new(_: &str) -> Awake {
            Awake
        }
    }
}

// ---- in the page ---------------------------------------------------------------
// each is an expression; `WebView::eval` runs it and hands back its value.

fn js(v: &str) -> String {
    serde_json::to_string(&v).unwrap_or_else(|_| "null".into())
}

/// a ring where the agent acts, fading: what it did, seen where it did it.
pub fn ring(x: f64, y: f64) -> String {
    format!(
        "(() => {{ for (const old of document.querySelectorAll('[data-tessel-ring]')) old.remove();
  const r = document.createElement('div'); r.dataset.tesselRing = '';
  r.style.cssText = 'position:fixed;left:{x}px;top:{y}px;width:20px;height:20px;margin:-10px 0 0 -10px;border:2.5px solid rgba(80,140,255,.95);border-radius:50%;pointer-events:none;z-index:2147483647;opacity:0';
  document.documentElement.appendChild(r);
  const a = r.animate([{{transform: 'scale(1)', opacity: 1}}, {{transform: 'scale(2.6)', opacity: 0}}], {{duration: 1000, easing: 'cubic-bezier(.2,.7,.3,1)'}});
  a.onfinish = () => r.remove(); setTimeout(() => r.remove(), 1500); return true; }})()"
    )
}

/// a person's click at a point: the pointer and mouse events a click makes,
/// focus where it lands, and the click itself (which follows links, ticks
/// boxes and submits forms like any click).
pub fn click(x: f64, y: f64) -> String {
    format!(
        "(() => {{ const x = {x}, y = {y}; const el = document.elementFromPoint(x, y); if (!el) return 'nothing is there';
  const o = {{bubbles: true, cancelable: true, composed: true, clientX: x, clientY: y, screenX: x, screenY: y, button: 0, view: window}};
  const p = {{...o, pointerId: 1, pointerType: 'mouse', isPrimary: true}};
  el.dispatchEvent(new PointerEvent('pointerover', p)); el.dispatchEvent(new MouseEvent('mouseover', o));
  el.dispatchEvent(new PointerEvent('pointerdown', {{...p, buttons: 1}}));
  if (el.dispatchEvent(new MouseEvent('mousedown', {{...o, buttons: 1}}))) {{
    const f = el.closest('input,textarea,select,button,a[href],[tabindex],[contenteditable=\"\"],[contenteditable=true]');
    if (f && f.focus) f.focus(); else if (document.activeElement && document.activeElement.blur) document.activeElement.blur();
  }}
  el.dispatchEvent(new PointerEvent('pointerup', p)); el.dispatchEvent(new MouseEvent('mouseup', o));
  el.dispatchEvent(new MouseEvent('click', {{...o, detail: 1}}));
  return true; }})()"
    )
}

/// text typed where the focus is, as typing would put it there.
pub fn type_text(text: &str) -> String {
    let t = js(text);
    format!(
        "(() => {{ const t = {t}; const el = document.activeElement;
  if (!el || el === document.body) return 'nothing on the page has the focus: click a field first';
  if (document.execCommand('insertText', false, t)) return true;
  if ('value' in el) {{
    const a = el.selectionStart ?? el.value.length, b = el.selectionEnd ?? el.value.length;
    const v = el.value.slice(0, a) + t + el.value.slice(b);
    const set = Object.getOwnPropertyDescriptor(Object.getPrototypeOf(el), 'value')?.set;
    set ? set.call(el, v) : (el.value = v);
    el.dispatchEvent(new InputEvent('input', {{bubbles: true, data: t, inputType: 'insertText'}}));
    el.dispatchEvent(new Event('change', {{bubbles: true}}));
    return true;
  }}
  return 'that element takes no text'; }})()"
    )
}

/// a named key pressed where the focus is, doing what it would.
pub fn key(name: &str) -> String {
    let (k, code, vk, _) = tessel_proto::key_codes(name);
    let (k, code) = if k.is_empty() { (name, name) } else { (k, code) };
    let (k, code) = (js(k), js(code));
    format!(
        "(() => {{ const el = document.activeElement || document.body; const k = {k};
  const o = {{key: k, code: {code}, keyCode: {vk}, which: {vk}, bubbles: true, cancelable: true, composed: true}};
  if (el.dispatchEvent(new KeyboardEvent('keydown', o))) {{
    if (k === 'Enter') {{
      if (el.tagName === 'TEXTAREA' || el.isContentEditable) document.execCommand('insertLineBreak');
      else if (el.form) el.form.requestSubmit ? el.form.requestSubmit() : el.form.submit();
      else if (el.tagName === 'BUTTON' || el.tagName === 'A') el.click();
    }} else if (k === 'Backspace') document.execCommand('delete');
    else if (k === 'Delete') document.execCommand('forwardDelete');
    else if (k === 'Tab') {{
      const all = [...document.querySelectorAll('a[href],button,input,select,textarea,[tabindex]:not([tabindex=\"-1\"]),[contenteditable=true]')].filter(e => !e.disabled && e.offsetParent !== null);
      const next = all[(all.indexOf(el) + 1) % all.length]; if (next) next.focus();
    }} else if (k === 'PageDown' || k === 'PageUp') (document.scrollingElement || document.documentElement).scrollBy(0, (k === 'PageDown' ? 1 : -1) * innerHeight * 0.9);
  }}
  el.dispatchEvent(new KeyboardEvent('keyup', o)); return true; }})()"
    )
}

/// scroll whatever scrolls under a point (the page itself, when nothing does).
pub fn scroll(x: Option<f64>, y: Option<f64>, dx: f64, dy: f64) -> String {
    let (x, y) = (x.map_or("innerWidth / 2".to_owned(), |v| v.to_string()), y.map_or("innerHeight / 2".to_owned(), |v| v.to_string()));
    format!(
        "(() => {{ let el = document.elementFromPoint({x}, {y});
  while (el && el !== document.body && el !== document.documentElement) {{
    const s = getComputedStyle(el);
    if (/(auto|scroll)/.test(s.overflowY + s.overflowX) && (el.scrollHeight > el.clientHeight || el.scrollWidth > el.clientWidth)) break;
    el = el.parentElement;
  }}
  const t = el && el !== document.body && el !== document.documentElement ? el : (document.scrollingElement || document.documentElement);
  t.scrollBy({{left: {dx}, top: {dy}, behavior: 'instant'}}); return true; }})()"
    )
}

/// point at an element: an outline follows the pointer, and the next click
/// (or Escape) is kept for `picked` instead of reaching the page.
pub fn pick_on() -> String {
    format!(
        "(() => {{ if (window.__tesselPick) return true; {css} {describe}
  const box = document.createElement('div');
  box.style.cssText = 'position:fixed;pointer-events:none;z-index:2147483647;border:2px solid rgba(80,140,255,.9);background:rgba(80,140,255,.14);border-radius:3px;display:none';
  document.documentElement.appendChild(box);
  const over = e => {{ const el = e.target; if (!el || el === box || el.nodeType !== 1) return; const r = el.getBoundingClientRect();
    Object.assign(box.style, {{display: 'block', left: r.x + 'px', top: r.y + 'px', width: r.width + 'px', height: r.height + 'px'}}); }};
  const stop = e => {{ e.preventDefault(); e.stopPropagation(); e.stopImmediatePropagation(); }};
  const kinds = ['pointerdown', 'pointerup', 'mousedown', 'mouseup', 'dblclick', 'auxclick', 'contextmenu'];
  const off = () => {{ removeEventListener('mouseover', over, true); for (const k of kinds) removeEventListener(k, stop, true);
    removeEventListener('click', click, true); removeEventListener('keydown', key, true); box.remove(); window.__tesselPick = null; }};
  const click = e => {{ stop(e); const el = e.target.nodeType === 1 ? e.target : e.target.parentElement; off(); window.__tesselPicked = describeEl(el); }};
  const key = e => {{ if (e.key === 'Escape') {{ stop(e); off(); window.__tesselPicked = {{cancelled: true}}; }} }};
  addEventListener('mouseover', over, true); for (const k of kinds) addEventListener(k, stop, true);
  addEventListener('click', click, true); addEventListener('keydown', key, true);
  window.__tesselPick = off; return true; }})()",
        css = tessel_proto::CSS_PATH,
        describe = tessel_proto::DESCRIBE
    )
}

pub const PICK_OFF: &str = "(() => { if (window.__tesselPick) window.__tesselPick(); return true; })()";

/// what was pointed at since last asked (taken, so it's said once), whether
/// pointing is still on, and where the page is.
pub const PICKED: &str = "(() => { const p = window.__tesselPicked || null; window.__tesselPicked = null; return {picked: p, on: !!window.__tesselPick}; })()";

#[cfg(test)]
mod tests {
    #[test]
    fn what_goes_into_a_script_stays_a_string() {
        let s = super::type_text("it's \"quoted\"\n</script>");
        assert!(s.contains(r#"const t = "it's \"quoted\"\n</script>";"#), "{s}");
        assert!(super::key("enter").contains("const k = \"Enter\""));
        assert!(super::key("a").contains("const k = \"a\""), "a key with no name of its own is itself");
        assert!(super::scroll(None, Some(10.0), 0.0, 300.0).contains("elementFromPoint(innerWidth / 2, 10)"));
    }
}
