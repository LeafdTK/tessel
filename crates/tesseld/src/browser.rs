//! the browser: a page per session in a Chromium the daemon runs, that the
//! person watches and steers from a pane and the agent drives with its tools.
//! both act on the one page, so whatever the agent does shows as it does it,
//! and the person can point at a part of the page to tell the agent about it.
//!
//! the daemon talks to the browser over the DevTools protocol, on a pipe of
//! its own (`--remote-debugging-pipe`): no port to find, no socket another
//! program could reach. the page is drawn off screen and sent to whoever is
//! watching as JPEG frames; clicks, scrolls and keys go back in as real input.
//!
//! any Chromium-family browser will do. none installed: tessel can download
//! Chrome for Testing, Google's build of Chrome for programs to drive, into its
//! own folder. Safari and Firefox can't be driven this way, so they're for
//! opening a page in, not this.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tessel_proto::{BrowserAct, BrowserEngine, BrowserState, CSS_PATH, DESCRIBE, Event, MouseKind, Picked, SessionId, key_codes, locate_script, read_script, url_of};
use tokio::sync::oneshot;

/// how the browser reaches the hub with what happened: frames, a page moving on, a pick.
pub type Emit = Arc<dyn Fn(Event) + Send + Sync>;

/// the browsers tried, in order, after tessel's own download.
const APPS: [(&str, &str); 5] = [
    ("Google Chrome", "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"),
    ("Chromium", "/Applications/Chromium.app/Contents/MacOS/Chromium"),
    ("Brave", "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"),
    ("Microsoft Edge", "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
    ("Vivaldi", "/Applications/Vivaldi.app/Contents/MacOS/Vivaldi"),
];

fn platform() -> &'static str {
    if cfg!(target_arch = "aarch64") { "mac-arm64" } else { "mac-x64" }
}

/// where an installed Chrome for Testing's program is.
fn installed(home: &Path) -> PathBuf {
    let p = platform();
    home.join("browser").join(format!("chrome-{p}")).join("Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing")
}

/// the browser to drive: `TESSEL_BROWSER`, tessel's own, else one of the person's.
pub fn find(home: &Path) -> Option<(String, PathBuf)> {
    if let Some(p) = std::env::var_os("TESSEL_BROWSER").map(PathBuf::from) {
        return p.is_file().then(|| ("Custom browser".to_owned(), p));
    }
    let own = installed(home);
    if own.is_file() {
        return Some(("Chrome for Testing".to_owned(), own));
    }
    APPS.iter().map(|(n, p)| (n.to_string(), PathBuf::from(p))).find(|(_, p)| p.is_file())
}

/// download Chrome for Testing into tessel's folder. `say` hears how it's going.
pub async fn install(home: &Path, say: impl Fn(&str)) -> Result<PathBuf, String> {
    let dir = home.join("browser");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    say("Finding the latest Chrome for Testing");
    let index = run("curl", &["-fsSL", "https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json"], &dir).await?;
    let index: Value = serde_json::from_str(&index).map_err(|e| format!("couldn't read the list of versions: {e}"))?;
    let url = index["channels"]["Stable"]["downloads"]["chrome"]
        .as_array()
        .and_then(|a| a.iter().find(|d| d["platform"] == platform()))
        .and_then(|d| d["url"].as_str())
        .ok_or("there's no Chrome for Testing for this Mac")?
        .to_owned();
    say("Downloading Chrome for Testing (about 150 MB)");
    let zip = dir.join("chrome.zip");
    run("curl", &["-fsSL", "-o", &zip.to_string_lossy(), &url], &dir).await?;
    say("Unpacking");
    run("unzip", &["-q", "-o", &zip.to_string_lossy(), "-d", &dir.to_string_lossy()], &dir).await?;
    let _ = std::fs::remove_file(&zip);
    let program = installed(home);
    if program.is_file() { Ok(program) } else { Err("the download didn't have Chrome in it".into()) }
}

async fn run(program: &str, args: &[&str], dir: &Path) -> Result<String, String> {
    let out = tokio::process::Command::new(program).args(args).current_dir(dir).stdin(std::process::Stdio::null()).output().await.map_err(|e| format!("{program}: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(format!("{program} failed: {}", String::from_utf8_lossy(&out.stderr).trim()))
    }
}

// ---- the protocol, over the pipe -----------------------------------------------------------

/// one connection to a running browser: calls answered by id, events passed on.
struct Cdp {
    out: Mutex<std::fs::File>,
    next: AtomicU64,
    pending: Mutex<HashMap<u64, oneshot::Sender<Result<Value, String>>>>,
    dead: AtomicBool,
}

impl Cdp {
    async fn call(&self, method: &str, params: Value, session: Option<&str>) -> Result<Value, String> {
        if self.dead.load(Ordering::Relaxed) {
            return Err("the browser has stopped".into());
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let mut msg = json!({"id": id, "method": method, "params": params});
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        let mut bytes = serde_json::to_vec(&msg).map_err(|e| e.to_string())?;
        bytes.push(0);
        self.out.lock().unwrap().write_all(&bytes).map_err(|e| format!("the browser isn't listening: {e}"))?;
        match tokio::time::timeout(Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err("the browser has stopped".into()),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                Err(format!("the browser didn't answer {method}"))
            }
        }
    }

    /// run a script on a page and have its value back.
    async fn eval(&self, session: &str, expression: &str) -> Result<Value, String> {
        let r = self.call("Runtime.evaluate", json!({"expression": expression, "returnByValue": true, "awaitPromise": true}), Some(session)).await?;
        if let Some(e) = r.get("exceptionDetails") {
            return Err(e["exception"]["description"].as_str().or(e["text"].as_str()).unwrap_or("the script failed").to_owned());
        }
        Ok(r["result"]["value"].clone())
    }
}

/// a session's page.
struct Tab {
    target: String,
    cdp: String,
    state: BrowserState,
    /// its size in CSS pixels, and the scale it's drawn at.
    size: (u32, u32, f64),
    watched: bool,
}

/// a running browser.
struct Chrome {
    cdp: Arc<Cdp>,
    tabs: Mutex<HashMap<SessionId, Tab>>,
    child: Mutex<Option<std::process::Child>>,
}

impl Drop for Chrome {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.lock().unwrap().take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Chrome {
    fn session_of(&self, cdp: &str) -> Option<SessionId> {
        self.tabs.lock().unwrap().iter().find(|(_, t)| t.cdp == cdp).map(|(s, _)| *s)
    }

    fn state(&self, session: SessionId) -> BrowserState {
        self.tabs.lock().unwrap().get(&session).map(|t| t.state.clone()).unwrap_or_default()
    }

    fn cdp_of(&self, session: SessionId) -> Option<String> {
        self.tabs.lock().unwrap().get(&session).map(|t| t.cdp.clone())
    }
}

/// the daemon's browser: started the first time a page is wanted.
pub struct Browser {
    home: PathBuf,
    emit: Emit,
    chrome: tokio::sync::Mutex<Option<Arc<Chrome>>>,
    installing: Mutex<Option<String>>,
}

/// highlight colours for pointing at an element: tessel's blue, quietly.
fn highlight() -> Value {
    json!({
        "showInfo": true,
        "contentColor": {"r": 80, "g": 140, "b": 255, "a": 0.18},
        "borderColor": {"r": 80, "g": 140, "b": 255, "a": 0.9},
        "paddingColor": {"r": 80, "g": 140, "b": 255, "a": 0.08},
        "marginColor": {"r": 255, "g": 170, "b": 60, "a": 0.08},
    })
}





/// a bare host is a web address.

impl Browser {
    pub fn new(home: PathBuf, emit: Emit) -> Self {
        Self { home, emit, chrome: tokio::sync::Mutex::new(None), installing: Mutex::new(None) }
    }

    pub fn engine(&self) -> BrowserEngine {
        let found = find(&self.home);
        BrowserEngine {
            name: found.as_ref().map(|(n, _)| n.clone()),
            path: found.map(|(_, p)| p.to_string_lossy().into_owned()),
            installing: self.installing.lock().unwrap().clone(),
        }
    }

    /// download Chrome for Testing, saying how it's going to every window.
    pub async fn install(&self) -> Result<BrowserEngine, String> {
        if self.installing.lock().unwrap().is_some() {
            return Err("it's being installed already".into());
        }
        let say = |what: &str| {
            *self.installing.lock().unwrap() = Some(what.to_owned());
            (self.emit)(Event::BrowserEngine { engine: self.engine() });
        };
        let result = install(&self.home, say).await;
        *self.installing.lock().unwrap() = None;
        (self.emit)(Event::BrowserEngine { engine: self.engine() });
        result.map(|_| self.engine())
    }

    /// the running browser, started if it isn't.
    async fn chrome(&self) -> Result<Arc<Chrome>, String> {
        let mut slot = self.chrome.lock().await;
        if let Some(c) = slot.as_ref().filter(|c| !c.cdp.dead.load(Ordering::Relaxed)) {
            return Ok(c.clone());
        }
        let (_, program) = find(&self.home).ok_or("there's no Chromium browser to use. install one from the browser pane")?;
        let chrome = self.start(&program).await?;
        *slot = Some(chrome.clone());
        Ok(chrome)
    }

    async fn start(&self, program: &Path) -> Result<Arc<Chrome>, String> {
        let profile = self.home.join("browser").join("profile");
        std::fs::create_dir_all(&profile).map_err(|e| e.to_string())?;
        // two pipes: it reads our calls on fd 3 and writes its answers to fd 4.
        let (mut to_chrome, mut from_chrome) = ([0i32; 2], [0i32; 2]);
        if unsafe { libc::pipe(to_chrome.as_mut_ptr()) } != 0 || unsafe { libc::pipe(from_chrome.as_mut_ptr()) } != 0 {
            return Err("couldn't make a pipe to the browser".into());
        }
        let (child_in, child_out) = (to_chrome[0], from_chrome[1]);
        let mut cmd = std::process::Command::new(program);
        cmd.args([
            "--headless=new",
            "--remote-debugging-pipe",
            &format!("--user-data-dir={}", profile.display()),
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--window-size=1280,800",
            "--force-device-scale-factor=2",
            "about:blank",
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
        unsafe {
            use std::os::unix::process::CommandExt;
            cmd.pre_exec(move || {
                if libc::dup2(child_in, 3) < 0 || libc::dup2(child_out, 4) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().map_err(|e| format!("couldn't start {}: {e}", program.display()))?;
        unsafe {
            libc::close(child_in);
            libc::close(child_out);
        }
        let out = unsafe { std::fs::File::from_raw_fd(to_chrome[1]) };
        let input = unsafe { std::fs::File::from_raw_fd(from_chrome[0]) };
        let cdp = Arc::new(Cdp { out: Mutex::new(out), next: AtomicU64::new(1), pending: Mutex::new(HashMap::new()), dead: AtomicBool::new(false) });
        let chrome = Arc::new(Chrome { cdp: cdp.clone(), tabs: Mutex::new(HashMap::new()), child: Mutex::new(Some(child)) });

        // what it says: answers to their calls, the rest to the event loop.
        let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel::<Value>();
        let reader_cdp = cdp.clone();
        std::thread::Builder::new()
            .name("browser-pipe".into())
            .spawn(move || {
                let mut r = std::io::BufReader::new(input);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match r.read_until(0, &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                    if buf.last() == Some(&0) {
                        buf.pop();
                    }
                    let Ok(msg) = serde_json::from_slice::<Value>(&buf) else { continue };
                    match msg.get("id").and_then(Value::as_u64) {
                        Some(id) => {
                            if let Some(tx) = reader_cdp.pending.lock().unwrap().remove(&id) {
                                let r = match msg.get("error") {
                                    Some(e) => Err(e["message"].as_str().unwrap_or("the browser refused").to_owned()),
                                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                                };
                                let _ = tx.send(r);
                            }
                        }
                        None => {
                            let _ = ev_tx.send(msg);
                        }
                    }
                }
                reader_cdp.dead.store(true, Ordering::Relaxed);
                reader_cdp.pending.lock().unwrap().clear();
            })
            .map_err(|e| e.to_string())?;
        let (weak, emit) = (Arc::downgrade(&chrome), self.emit.clone());
        tokio::spawn(async move {
            while let Some(msg) = ev_rx.recv().await {
                let Some(chrome) = weak.upgrade() else { break };
                on_event(&chrome, &emit, msg).await;
            }
        });
        // pages' titles as they change.
        cdp.call("Target.setDiscoverTargets", json!({"discover": true}), None).await?;
        Ok(chrome)
    }

    /// the session's page, made the first time (at `first_url`).
    async fn tab(&self, chrome: &Chrome, session: SessionId, first_url: &str) -> Result<String, String> {
        if let Some(cdp) = chrome.cdp_of(session) {
            return Ok(cdp);
        }
        let made = chrome.cdp.call("Target.createTarget", json!({"url": "about:blank"}), None).await?;
        let target = made["targetId"].as_str().ok_or("the browser made no page")?.to_owned();
        let attached = chrome.cdp.call("Target.attachToTarget", json!({"targetId": target, "flatten": true}), None).await?;
        let cdp = attached["sessionId"].as_str().ok_or("couldn't reach the page")?.to_owned();
        for m in ["Page.enable", "DOM.enable", "Overlay.enable", "Runtime.enable"] {
            chrome.cdp.call(m, json!({}), Some(&cdp)).await?;
        }
        let size = (1280, 800, 2.0);
        chrome.cdp.call("Emulation.setDeviceMetricsOverride", json!({"width": size.0, "height": size.1, "deviceScaleFactor": size.2, "mobile": false}), Some(&cdp)).await?;
        chrome.tabs.lock().unwrap().insert(session, Tab { target, cdp: cdp.clone(), state: BrowserState::default(), size, watched: false });
        chrome.cdp.call("Page.navigate", json!({"url": url_of(first_url)}), Some(&cdp)).await?;
        Ok(cdp)
    }

    /// forget a session's page (the session went).
    pub async fn close(&self, session: SessionId) {
        let chrome = self.chrome.lock().await.clone();
        if let Some(chrome) = chrome {
            let target = chrome.tabs.lock().unwrap().remove(&session).map(|t| t.target);
            if let Some(target) = target {
                let _ = chrome.cdp.call("Target.closeTarget", json!({"targetId": target}), None).await;
            }
        }
    }

    /// do something to a session's page. `first_url`: where a new page starts.
    /// what came of it: where the page is, and what reading or a screenshot found.
    pub async fn act(&self, session: SessionId, act: BrowserAct, agent: bool, first_url: &str) -> Result<(BrowserState, Option<String>, Option<String>), String> {
        let chrome = self.chrome().await?;
        // stopping frames to a page that isn't there needs no page made for it.
        if matches!(act, BrowserAct::Watch { on: false }) && chrome.cdp_of(session).is_none() {
            return Ok((BrowserState::default(), None, None));
        }
        let page = self.tab(&chrome, session, first_url).await?;
        let cdp = &chrome.cdp;
        let tell = |what: String, x: Option<f64>, y: Option<f64>| {
            if agent {
                (self.emit)(Event::BrowserActivity { session, what, x, y });
            }
        };
        let (mut text, mut image) = (None, None);
        match act {
            BrowserAct::Open { url } => {
                if let Some(url) = url {
                    cdp.call("Page.navigate", json!({"url": url_of(&url)}), Some(&page)).await?;
                    tell(format!("went to {}", url_of(&url)), None, None);
                    loaded(cdp, &page).await;
                }
            }
            BrowserAct::Navigate { url } => {
                cdp.call("Page.navigate", json!({"url": url_of(&url)}), Some(&page)).await?;
                tell(format!("went to {}", url_of(&url)), None, None);
                loaded(cdp, &page).await;
            }
            BrowserAct::Back | BrowserAct::Forward => {
                let h = cdp.call("Page.getNavigationHistory", json!({}), Some(&page)).await?;
                let at = h["currentIndex"].as_i64().unwrap_or(0) + if matches!(act, BrowserAct::Back) { -1 } else { 1 };
                if let Some(entry) = h["entries"].as_array().and_then(|e| usize::try_from(at).ok().and_then(|i| e.get(i))) {
                    cdp.call("Page.navigateToHistoryEntry", json!({"entryId": entry["id"]}), Some(&page)).await?;
                    tell(if matches!(act, BrowserAct::Back) { "went back".into() } else { "went forward".into() }, None, None);
                    loaded(cdp, &page).await;
                }
            }
            BrowserAct::Reload => {
                cdp.call("Page.reload", json!({}), Some(&page)).await?;
                tell("reloaded".into(), None, None);
                loaded(cdp, &page).await;
            }
            BrowserAct::Click { selector, x, y } => {
                let (x, y, label) = match (selector, x, y) {
                    (Some(sel), _, _) => {
                        let at = cdp.eval(&page, &locate_script(&sel)).await?;
                        if at.is_null() {
                            return Err(format!("there's no {sel} on the page"));
                        }
                        (at["x"].as_f64().unwrap_or(0.0), at["y"].as_f64().unwrap_or(0.0), at["label"].as_str().unwrap_or(&sel).to_owned())
                    }
                    (None, Some(x), Some(y)) => (x, y, format!("at {x:.0}, {y:.0}")),
                    _ => return Err("click needs a selector, or x and y".into()),
                };
                tell(format!("clicked {label}"), Some(x), Some(y));
                click(cdp, &page, x, y).await?;
            }
            BrowserAct::Type { text: t, selector } => {
                if let Some(sel) = selector {
                    let at = cdp.eval(&page, &locate_script(&sel)).await?;
                    if at.is_null() {
                        return Err(format!("there's no {sel} on the page"));
                    }
                    let (x, y) = (at["x"].as_f64().unwrap_or(0.0), at["y"].as_f64().unwrap_or(0.0));
                    tell(format!("typed into {}", at["label"].as_str().unwrap_or(&sel)), Some(x), Some(y));
                    click(cdp, &page, x, y).await?;
                } else {
                    tell("typed".into(), None, None);
                }
                cdp.call("Input.insertText", json!({"text": t}), Some(&page)).await?;
            }
            BrowserAct::Key { key } => {
                let (k, code, vk, txt) = key_codes(&key);
                let key_name = if k.is_empty() { key.clone() } else { k.to_owned() };
                let mut down = json!({"type": "keyDown", "key": key_name, "code": if code.is_empty() { key.as_str() } else { code }, "windowsVirtualKeyCode": vk});
                if let Some(t) = txt {
                    down["text"] = json!(t);
                }
                cdp.call("Input.dispatchKeyEvent", down, Some(&page)).await?;
                cdp.call("Input.dispatchKeyEvent", json!({"type": "keyUp", "key": key_name, "code": if code.is_empty() { key.as_str() } else { code }, "windowsVirtualKeyCode": vk}), Some(&page)).await?;
                tell(format!("pressed {key_name}"), None, None);
            }
            BrowserAct::Scroll { dx, dy, x, y } => {
                let size = chrome.tabs.lock().unwrap().get(&session).map(|t| t.size).unwrap_or((1280, 800, 2.0));
                let (x, y) = (x.unwrap_or(f64::from(size.0) / 2.0), y.unwrap_or(f64::from(size.1) / 2.0));
                cdp.call("Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy}), Some(&page)).await?;
                tell("scrolled".into(), None, None);
            }
            BrowserAct::Read => {
                tell("read the page".into(), None, None);
                text = Some(cdp.eval(&page, &read_script()).await?.as_str().unwrap_or_default().to_owned());
            }
            BrowserAct::Screenshot => {
                tell("looked at the page".into(), None, None);
                let shot = cdp.call("Page.captureScreenshot", json!({"format": "png"}), Some(&page)).await?;
                image = shot["data"].as_str().map(str::to_owned);
            }
            BrowserAct::Watch { on } => {
                let (was, size) = {
                    let mut tabs = chrome.tabs.lock().unwrap();
                    let t = tabs.get_mut(&session).ok_or("no page")?;
                    (std::mem::replace(&mut t.watched, on), t.size)
                };
                if on && !was {
                    screencast(cdp, &page, size).await?;
                } else if !on && was {
                    cdp.call("Page.stopScreencast", json!({}), Some(&page)).await?;
                }
            }
            BrowserAct::Resize { width, height, scale } => {
                let size = (width.max(200), height.max(150), scale.clamp(1.0, 3.0));
                let watched = {
                    let mut tabs = chrome.tabs.lock().unwrap();
                    let t = tabs.get_mut(&session).ok_or("no page")?;
                    t.size = size;
                    t.watched
                };
                cdp.call("Emulation.setDeviceMetricsOverride", json!({"width": size.0, "height": size.1, "deviceScaleFactor": size.2, "mobile": false}), Some(&page)).await?;
                if watched {
                    cdp.call("Page.stopScreencast", json!({}), Some(&page)).await?;
                    screencast(cdp, &page, size).await?;
                }
            }
            BrowserAct::Pick { on } => {
                let mode = if on { "searchForNode" } else { "none" };
                cdp.call("Overlay.setInspectMode", json!({"mode": mode, "highlightConfig": highlight()}), Some(&page)).await?;
                if let Some(t) = chrome.tabs.lock().unwrap().get_mut(&session) {
                    t.state.picking = on;
                }
                (self.emit)(Event::BrowserState { session, state: chrome.state(session) });
            }
            BrowserAct::Mouse { kind, x, y, clicks, modifiers } => {
                let (ty, button) = match kind {
                    MouseKind::Move => ("mouseMoved", "none"),
                    MouseKind::Down => ("mousePressed", "left"),
                    MouseKind::Up => ("mouseReleased", "left"),
                };
                cdp.call("Input.dispatchMouseEvent", json!({"type": ty, "x": x, "y": y, "button": button, "clickCount": clicks.max(u32::from(kind != MouseKind::Move)), "modifiers": modifiers}), Some(&page)).await?;
            }
            BrowserAct::Wheel { x, y, dx, dy } => {
                cdp.call("Input.dispatchMouseEvent", json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy}), Some(&page)).await?;
            }
        }
        Ok((chrome.state(session), text, image))
    }
}

/// frames of a page, at its size, to whoever watches.
async fn screencast(cdp: &Cdp, page: &str, size: (u32, u32, f64)) -> Result<(), String> {
    let (w, h) = ((f64::from(size.0) * size.2) as u32, (f64::from(size.1) * size.2) as u32);
    cdp.call("Page.startScreencast", json!({"format": "jpeg", "quality": 80, "maxWidth": w, "maxHeight": h, "everyNthFrame": 1}), Some(page)).await.map(drop)
}

async fn click(cdp: &Cdp, page: &str, x: f64, y: f64) -> Result<(), String> {
    cdp.call("Input.dispatchMouseEvent", json!({"type": "mouseMoved", "x": x, "y": y}), Some(page)).await?;
    cdp.call("Input.dispatchMouseEvent", json!({"type": "mousePressed", "x": x, "y": y, "button": "left", "clickCount": 1}), Some(page)).await?;
    cdp.call("Input.dispatchMouseEvent", json!({"type": "mouseReleased", "x": x, "y": y, "button": "left", "clickCount": 1}), Some(page)).await.map(drop)
}

/// wait for a page to finish loading, up to ten seconds.
async fn loaded(cdp: &Cdp, page: &str) {
    for _ in 0..100 {
        if cdp.eval(page, "document.readyState").await.ok().and_then(|v| v.as_str().map(|s| s == "complete")).unwrap_or(false) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// what the browser says without being asked.
async fn on_event(chrome: &Arc<Chrome>, emit: &Emit, msg: Value) {
    let method = msg["method"].as_str().unwrap_or_default();
    let params = &msg["params"];
    let cdp_session = msg["sessionId"].as_str().unwrap_or_default();
    let session = chrome.session_of(cdp_session);
    let changed = |chrome: &Chrome, session: SessionId, f: &dyn Fn(&mut BrowserState)| {
        let state = {
            let mut tabs = chrome.tabs.lock().unwrap();
            let Some(t) = tabs.get_mut(&session) else { return };
            f(&mut t.state);
            t.state.clone()
        };
        emit(Event::BrowserState { session, state });
    };
    match method {
        "Page.screencastFrame" => {
            let ack = params["sessionId"].clone();
            let (cdp, page) = (chrome.cdp.clone(), cdp_session.to_owned());
            tokio::spawn(async move {
                let _ = cdp.call("Page.screencastFrameAck", json!({"sessionId": ack}), Some(&page)).await;
            });
            if let (Some(session), Some(jpeg)) = (session, params["data"].as_str()) {
                let m = &params["metadata"];
                let (w, h) = (m["deviceWidth"].as_f64().unwrap_or(0.0) as u32, m["deviceHeight"].as_f64().unwrap_or(0.0) as u32);
                emit(Event::BrowserFrame { session, jpeg: jpeg.to_owned(), width: w, height: h });
            }
        }
        "Page.frameNavigated" if params["frame"]["parentId"].is_null() => {
            if let Some(s) = session {
                let url = params["frame"]["url"].as_str().unwrap_or_default().to_owned();
                changed(chrome, s, &|st| st.url = url.clone());
            }
        }
        "Page.frameStartedLoading" => {
            if let Some(s) = session {
                changed(chrome, s, &|st| st.loading = true);
            }
        }
        "Page.loadEventFired" | "Page.frameStoppedLoading" => {
            if let Some(s) = session {
                changed(chrome, s, &|st| st.loading = false);
            }
        }
        "Target.targetInfoChanged" => {
            let info = &params["targetInfo"];
            let target = info["targetId"].as_str().unwrap_or_default();
            let found = chrome.tabs.lock().unwrap().iter().find(|(_, t)| t.target == target).map(|(s, _)| *s);
            if let Some(s) = found {
                let (title, url) = (info["title"].as_str().unwrap_or_default().to_owned(), info["url"].as_str().unwrap_or_default().to_owned());
                changed(chrome, s, &|st| {
                    st.title = title.clone();
                    st.url = url.clone();
                });
            }
        }
        "Overlay.inspectNodeRequested" => {
            if let Some(s) = session {
                let (chrome, emit, page, node) = (chrome.clone(), emit.clone(), cdp_session.to_owned(), params["backendNodeId"].clone());
                tokio::spawn(async move {
                    if let Ok(picked) = pick(&chrome.cdp, &page, node).await {
                        emit(Event::BrowserPicked { session: s, picked });
                    }
                    let _ = chrome.cdp.call("Overlay.setInspectMode", json!({"mode": "none", "highlightConfig": highlight()}), Some(&page)).await;
                    let state = {
                        let mut tabs = chrome.tabs.lock().unwrap();
                        tabs.get_mut(&s).map(|t| {
                            t.state.picking = false;
                            t.state.clone()
                        })
                    };
                    if let Some(state) = state {
                        emit(Event::BrowserState { session: s, state });
                    }
                });
            }
        }
        _ => {}
    }
}

/// what was pointed at: where it is, what it is, and a picture of it.
async fn pick(cdp: &Cdp, page: &str, node: Value) -> Result<Picked, String> {
    let object = cdp.call("DOM.resolveNode", json!({"backendNodeId": node}), Some(page)).await?;
    let id = object["object"]["objectId"].as_str().ok_or("that isn't an element")?;
    let describe = format!("function() {{ {CSS_PATH} {DESCRIBE} return describeEl(this.nodeType === 1 ? this : this.parentElement); }}");
    let r = cdp.call("Runtime.callFunctionOn", json!({"objectId": id, "functionDeclaration": describe, "returnByValue": true}), Some(page)).await?;
    let v = &r["result"]["value"];
    let num = |k: &str| v[k].as_f64().unwrap_or(0.0);
    let pad = 6.0;
    let clip = json!({"x": (num("x") - pad).max(0.0), "y": (num("y") - pad).max(0.0), "width": num("width") + pad * 2.0, "height": num("height") + pad * 2.0, "scale": 1});
    let shot = cdp.call("Page.captureScreenshot", json!({"format": "png", "clip": clip, "captureBeyondViewport": true}), Some(page)).await?;
    Ok(Picked::from_described(v, shot["data"].as_str().unwrap_or_default().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_address_is_a_web_address_and_localhost_is_plain() {
        assert_eq!(url_of("example.com"), "https://example.com");
        assert_eq!(url_of("localhost:41000/x"), "http://localhost:41000/x");
        assert_eq!(url_of("http://a.b"), "http://a.b");
        assert_eq!(url_of("about:blank"), "about:blank");
        assert_eq!(key_codes("enter").2, 13);
        assert_eq!(key_codes("Escape").0, "Escape");
    }

    /// drives a real browser, so it runs where there is one (TESSEL_TEST_BROWSER=1).
    #[tokio::test]
    async fn the_agent_and_the_person_use_the_same_page() {
        if std::env::var_os("TESSEL_TEST_BROWSER").is_none() {
            return;
        }
        let home = std::env::temp_dir().join(format!("tessel-browser-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let seen: Arc<Mutex<Vec<Event>>> = Arc::default();
        let log = seen.clone();
        let b = Browser::new(home.clone(), Arc::new(move |e| log.lock().unwrap().push(e)));
        let s = SessionId::new();
        let page = "data:text/html,<title>t</title><button id=go onclick=\"document.title='clicked'\">Go</button><input id=name>";
        b.act(s, BrowserAct::Open { url: Some(page.into()) }, true, "about:blank").await.unwrap();
        let (_, text, _) = b.act(s, BrowserAct::Read, true, "").await.unwrap();
        assert!(text.unwrap().contains("#go"), "the page's buttons, with selectors");
        b.act(s, BrowserAct::Click { selector: Some("#go".into()), x: None, y: None }, true, "").await.unwrap();
        b.act(s, BrowserAct::Type { text: "hi".into(), selector: Some("#name".into()) }, true, "").await.unwrap();
        let chrome = b.chrome().await.unwrap();
        let page_id = chrome.cdp_of(s).unwrap();
        assert_eq!(chrome.cdp.eval(&page_id, "document.title + ':' + document.querySelector('#name').value").await.unwrap(), "clicked:hi");
        let (_, _, image) = b.act(s, BrowserAct::Screenshot, true, "").await.unwrap();
        assert!(image.unwrap().len() > 100);
        // frames, once someone watches.
        b.act(s, BrowserAct::Watch { on: true }, false, "").await.unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        assert!(seen.lock().unwrap().iter().any(|e| matches!(e, Event::BrowserFrame { .. })), "frames arrive");
        // a retina pane gets retina frames: twice the page's size in pixels.
        b.act(s, BrowserAct::Resize { width: 640, height: 400, scale: 2.0 }, false, "").await.unwrap();
        chrome.cdp.eval(&page_id, "document.body.style.background='rgb(200,10,10)'").await.unwrap();
        tokio::time::sleep(Duration::from_millis(800)).await;
        let last = seen.lock().unwrap().iter().rev().find_map(|e| match e {
            Event::BrowserFrame { jpeg, width, .. } => Some((jpeg.clone(), *width)),
            _ => None,
        });
        let (jpeg, css) = last.expect("a frame after resizing");
        use base64::Engine as _;
        let bytes = base64::engine::general_purpose::STANDARD.decode(jpeg).unwrap();
        // the size, from the JPEG's start-of-frame marker.
        let sof = bytes.windows(2).position(|w| w[0] == 0xFF && (w[1] == 0xC0 || w[1] == 0xC2)).unwrap();
        let (h, w) = (u16::from_be_bytes([bytes[sof + 5], bytes[sof + 6]]), u16::from_be_bytes([bytes[sof + 7], bytes[sof + 8]]));
        eprintln!("frame {w}x{h} for a page {css} wide");
        assert_eq!((w, h, css), (1280, 800, 640));
        // as React's development build leaves it on an element.
        let fiber = "(() => { const b = document.querySelector('#go'); function Button() {} function LoginForm() {}
          b.__reactFiber$x = {type: 'button', return: {type: Button, _debugSource: {fileName: 'webpack://app/src/Button.tsx', lineNumber: 7}, return: {type: LoginForm}}}; return true; })()";
        chrome.cdp.eval(&page_id, fiber).await.unwrap();
        // pointing at the button picks it.
        b.act(s, BrowserAct::Pick { on: true }, false, "").await.unwrap();
        let at = chrome.cdp.eval(&page_id, &locate_script("#go")).await.unwrap();
        let (x, y) = (at["x"].as_f64().unwrap(), at["y"].as_f64().unwrap());
        for kind in [MouseKind::Move, MouseKind::Down, MouseKind::Up] {
            b.act(s, BrowserAct::Mouse { kind, x, y, clicks: 1, modifiers: 0 }, false, "").await.unwrap();
        }
        tokio::time::sleep(Duration::from_millis(800)).await;
        let picked = seen.lock().unwrap().iter().find_map(|e| match e {
            Event::BrowserPicked { picked, .. } => Some(picked.clone()),
            _ => None,
        });
        let picked = picked.expect("picked");
        assert_eq!(picked.selector, "#go");
        assert!(picked.html.starts_with("<button") && !picked.image.is_empty());
        assert_eq!((picked.tag.as_str(), picked.name.as_str()), ("button", "Go"));
        // a development build says where the element was made, and in what.
        assert_eq!((picked.source.as_str(), picked.components.as_str()), ("src/Button.tsx:7", "Button > LoginForm"));
        assert!(picked.note().contains("source: src/Button.tsx:7 (in Button > LoginForm)"), "{}", picked.note());
        assert!(seen.lock().unwrap().iter().any(|e| matches!(e, Event::BrowserActivity { what, .. } if what.starts_with("clicked"))), "watchers see the agent's clicks");
        drop(chrome);
        drop(b);
        let _ = std::fs::remove_dir_all(&home);
    }
}
