//! voice in the message box. hold ⌥ and talk; let go and what you said is in
//! the box, at the cursor, for you to send or fix first. for something long,
//! ⇧⌥ (or the mic) keeps listening with your hands free, until Done (or ⇧⌥
//! again); adding ⇧ while holding ⌥ does the same mid-sentence. esc throws a
//! take away.
//!
//! the listening is `tessel-voice`'s, a small helper beside the app: the
//! microphone, and the speech engines that only exist in swift (apple's own,
//! on this Mac). it's started the first time it's needed and kept; it does
//! nothing while you aren't talking. this side tells it start, stop and
//! cancel, and shows what it hears as it hears it: a waveform, and the words.

use super::*;
use std::collections::VecDeque;
use std::io::BufRead;
#[cfg(not(test))]
use std::io::Write as _;

/// how long ⌥ is held on its own before it counts as talking: long enough
/// that ⌥ with a key (é, ø, …) never starts anything.
const HOLD: Duration = Duration::from_millis(180);
/// the waveform's bars.
const BARS: usize = 36;

/// what the helper says.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Heard {
    Ready,
    /// getting ready the first time: a model coming down, and how far.
    Preparing(String, f32),
    Listening,
    Level(f32),
    /// everything heard so far, still changing.
    Partial(String),
    Final(String),
    Cancelled,
    Error(String),
    /// the helper went away.
    Gone,
}

pub(crate) fn heard(line: &str) -> Option<Heard> {
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let s = |k: &str| v.get(k).and_then(serde_json::Value::as_str).unwrap_or_default().to_owned();
    Some(match v.get("ev")?.as_str()? {
        "ready" => Heard::Ready,
        "preparing" => Heard::Preparing(s("what"), v.get("progress").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32),
        "listening" => Heard::Listening,
        "level" => Heard::Level(v.get("level").and_then(serde_json::Value::as_f64).unwrap_or(0.0) as f32),
        "partial" => Heard::Partial(s("text")),
        "final" => Heard::Final(s("text")),
        "cancelled" => Heard::Cancelled,
        "error" => Heard::Error(s("message")),
        _ => return None,
    })
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Listen {
    Off,
    /// asked to start; not listening yet.
    Waking,
    Preparing(String, f32),
    On,
    /// let go: the last words are being worked out.
    Finishing,
}

pub(crate) struct Dictation {
    pub(crate) state: Listen,
    pub(crate) words: String,
    pub(crate) levels: VecDeque<f32>,
    /// started by holding ⌥: it ends when ⌥ is let go.
    by_key: bool,
    /// ⌥ went down on its own, then: it becomes talking if nothing else is pressed first.
    alt_since: Option<Instant>,
    alt_spoiled: bool,
    /// ⇧ came with it: once started, it keeps listening after they're let go.
    alt_hands_free: bool,
    /// ⇧⌥ was down last time the modifiers changed: pressing it again (not holding it) ends a hands-free take.
    chord_down: bool,
    link: Option<std::process::ChildStdin>,
    /// the helper's pid: stopped if it ever stops answering.
    pid: Option<u32>,
    /// what went in the box, being tidied by a model.
    pub(crate) tidying: Option<String>,
    /// a tidied version, and what it takes the place of: done at the next frame.
    swap: Option<(String, String)>,
    /// what the helper was told, in a test (there's no helper there).
    #[cfg(test)]
    pub(crate) told: Vec<String>,
}

impl Dictation {
    pub(crate) fn new() -> Self {
        Self {
            state: Listen::Off,
            words: String::new(),
            levels: VecDeque::new(),
            by_key: false,
            alt_since: None,
            alt_spoiled: false,
            alt_hands_free: false,
            chord_down: false,
            link: None,
            pid: None,
            tidying: None,
            swap: None,
            #[cfg(test)]
            told: Vec::new(),
        }
    }

    pub(crate) fn active(&self) -> bool {
        self.state != Listen::Off
    }
}

/// `~/.tessel/voice.log`: what was said to the helper, what it said back, and
/// what it noted on the way (its stderr). levels left out: they're many.
fn voice_log(what: &str) {
    use std::io::Write;
    let path = tessel_proto::local::home().join("voice.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {what}", chrono_now());
    }
}

/// the time of day, for the log.
fn chrono_now() -> String {
    let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
    let s = (ms / 1000) % 86_400;
    format!("{:02}:{:02}:{:02}.{:03}Z", s / 3600, (s / 60) % 60, s % 60, ms % 1000)
}

/// where the helper is: beside the app (in its bundle), else built in the source tree.
fn helper_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("TESSEL_VOICE_BIN") {
        return Some(p.into());
    }
    let beside = std::env::current_exe().ok()?.parent()?.join("tessel-voice");
    if beside.is_file() {
        return Some(beside);
    }
    let tree = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../voice/.build");
    ["release", "debug"].into_iter().map(|c| tree.join(c).join("tessel-voice")).find(|p| p.is_file())
}

impl Shell {
    /// tell the helper something, starting it first if it isn't running.
    fn tell_voice(&mut self, msg: serde_json::Value, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let line = format!("{msg}\n");
        #[cfg(test)]
        {
            let _ = (window, cx);
            self.dictation.told.push(line.trim().to_owned());
            Ok(())
        }
        #[cfg(not(test))]
        {
            if self.dictation.link.is_none() {
                self.start_voice_helper(window, cx)?;
            }
            voice_log(&format!("→ {}", line.trim()));
            let ok = self.dictation.link.as_mut().is_some_and(|w| w.write_all(line.as_bytes()).and_then(|()| w.flush()).is_ok());
            if !ok {
                self.dictation.link = None;
                return Err("The voice helper stopped. Try again.".into());
            }
            Ok(())
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    fn start_voice_helper(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let path = helper_path().ok_or("Voice input needs tessel-voice beside Tessel, which this build doesn't have (it needs macOS 26's SDK to build).")?;
        let mut child = std::process::Command::new(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::fs::OpenOptions::new().create(true).append(true).open(tessel_proto::local::home().join("voice.log")).map_or_else(|_| std::process::Stdio::null(), std::process::Stdio::from))
            .spawn()
            .map_err(|e| format!("Couldn't start the voice helper: {e}"))?;
        let stdout = child.stdout.take().ok_or("the voice helper has no output")?;
        self.dictation.link = child.stdin.take();
        self.dictation.pid = Some(child.id());
        let (tx, mut rx) = mpsc::unbounded_channel::<Heard>();
        std::thread::Builder::new()
            .name("tessel-voice".into())
            .spawn(move || {
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    if !line.contains("\"level\"") {
                        voice_log(&format!("← {line}"));
                    }
                    if let Some(h) = heard(&line)
                        && tx.send(h).is_err()
                    {
                        break;
                    }
                }
                voice_log("the helper went");
                let _ = tx.send(Heard::Gone);
                let _ = child.wait();
            })
            .map_err(|e| e.to_string())?;
        cx.spawn_in(window, async move |this, cx| {
            while let Some(h) = rx.recv().await {
                // a burst of levels and partials is one redraw.
                let mut batch = vec![h];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let alive = in_window(&this, cx, |this, window, cx| {
                    for h in batch {
                        this.voice_heard(h, window, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        Ok(())
    }

    /// start listening. `by_key`: it was ⌥, and letting go of ⌥ ends it.
    pub(crate) fn dictate_start(&mut self, by_key: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictation.active() {
            return;
        }
        self.dictation.by_key = by_key;
        self.dictation.words.clear();
        self.dictation.levels.clear();
        match self.tell_voice(serde_json::json!({"op": "start", "engine": "apple"}), window, cx) {
            Ok(()) => self.dictation.state = Listen::Waking,
            Err(e) => self.error = Some(e),
        }
        cx.notify();
    }

    /// done talking: what was said lands in the box once it's worked out.
    pub(crate) fn dictate_stop(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dictation.active() || self.dictation.state == Listen::Finishing {
            return;
        }
        self.dictation.state = Listen::Finishing;
        if let Err(e) = self.tell_voice(serde_json::json!({"op": "stop"}), window, cx) {
            self.error = Some(e);
            self.dictation.state = Listen::Off;
        }
        // it always answers a stop within seconds; if it doesn't, it's stuck, and it goes.
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(10)).await;
            let _ = in_window(&this, cx, |this, _, cx| {
                if this.dictation.state == Listen::Finishing {
                    voice_log("no answer to stop in 10s: stopping the helper");
                    this.dictation.state = Listen::Off;
                    this.dictation.link = None;
                    if let Some(pid) = this.dictation.pid.take() {
                        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
                    }
                    this.error = Some("Voice input stopped answering, so it was restarted. Try again (~/.tessel/voice.log says what happened).".into());
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    pub(crate) fn dictate_cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.dictation.active() {
            return;
        }
        self.dictation.state = Listen::Off;
        let _ = self.tell_voice(serde_json::json!({"op": "cancel"}), window, cx);
        cx.notify();
    }

    pub(crate) fn dictate_toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dictation.active() {
            self.dictate_stop(window, cx);
        } else {
            self.dictate_start(false, window, cx);
            self.composer.update(cx, |c, cx| c.focus(window, cx));
        }
    }

    pub(crate) fn voice_heard(&mut self, h: Heard, window: &mut Window, cx: &mut Context<Self>) {
        let d = &mut self.dictation;
        match h {
            Heard::Ready => {}
            Heard::Preparing(what, progress) if d.active() => d.state = Listen::Preparing(what, progress),
            Heard::Listening if d.active() && d.state != Listen::Finishing => d.state = Listen::On,
            Heard::Level(l) if d.active() => {
                d.levels.push_back(l);
                while d.levels.len() > BARS {
                    d.levels.pop_front();
                }
            }
            Heard::Partial(text) if d.active() => d.words = text,
            Heard::Final(text) if d.active() => {
                d.state = Listen::Off;
                d.words.clear();
                d.levels.clear();
                self.dictated(text, window, cx);
            }
            Heard::Cancelled => d.state = Listen::Off,
            Heard::Error(message) => {
                d.state = Listen::Off;
                self.error = Some(message);
            }
            Heard::Gone => {
                d.state = Listen::Off;
                d.link = None;
                d.pid = None;
            }
            _ => {}
        }
        cx.notify();
    }

    /// what was said goes in the box at the cursor, a space from what's beside it.
    /// at once with the noises left out; then, once a model has tidied it,
    /// swapped for the tidied version, unless it's been changed meanwhile.
    fn dictated(&mut self, said: String, window: &mut Window, cx: &mut Context<Self>) {
        let plain = tessel_proto::voice::strip_noises(said.trim());
        if plain.is_empty() {
            return;
        }
        let before = self.composer.read(cx).value().to_string();
        let gap = if before.is_empty() || before.ends_with(char::is_whitespace) { "" } else { " " };
        // carrying on a sentence already started: no capital at its first word.
        let mid_sentence = !before.trim_end().is_empty() && !before.trim_end().ends_with(['.', '?', '!', '\n', ':']);
        let plain = if mid_sentence { lower_first(&plain) } else { plain };
        let words = format!("{gap}{plain}");
        self.composer.update(cx, |c, cx| {
            c.insert(words, window, cx);
            c.focus(window, cx);
        });
        if plain.split_whitespace().count() >= 4 {
            self.dictation.tidying = Some(plain);
            let session = self.model.open_summary().filter(|_| !self.show_new).map(|s| s.id);
            self.cmd(Cmd::Ask { tag: "voice.tidy", req: tessel_proto::Request::VoiceTidy { session, text: said } });
        }
    }

    /// the tidied version is here: it takes the place of what went in, if that's still there as it was.
    pub(crate) fn voice_answer(&mut self, result: Result<tessel_proto::Response, String>, cx: &mut Context<Self>) {
        let Some(plain) = self.dictation.tidying.take() else { return };
        if let Ok(tessel_proto::Response::Tidied { text, by }) = result {
            voice_log(&format!("tidied by {by}"));
            if text.trim() != plain {
                self.dictation.swap = Some((plain, text.trim().to_owned()));
            }
        }
        cx.notify();
    }

    /// once a frame (setting the box's text needs the window): a tidied version in its place.
    pub(crate) fn sync_dictation(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((plain, tidied)) = self.dictation.swap.take() else { return };
        let now = self.composer.read(cx).value().to_string();
        // the last place it went in; if it's been edited away, what's there stays.
        let Some(at) = now.rfind(&plain) else { return };
        let next = format!("{}{tidied}{}", &now[..at], &now[at + plain.len()..]);
        let lines = next.split('\n').count().saturating_sub(1) as u32;
        let last = next.rsplit('\n').next().map_or(0, |l| l.chars().count() as u32);
        self.composer.update(cx, |c, cx| {
            c.set_value(next, window, cx);
            c.set_cursor_position(gpui_kit::component::input::Position { line: lines, character: last }, window, cx);
        });
    }

    /// ⌥ on its own, held in the message box, is talking until it's let go;
    /// ⇧⌥ is talking until Done (or ⇧⌥ again). anything pressed with them
    /// first makes them the shortcut they always were.
    pub(crate) fn dictate_modifiers(&mut self, m: &Modifiers, window: &mut Window, cx: &mut Context<Self>) {
        let others = m.control || m.platform || m.function;
        let alt_only = m.alt && !m.shift && !others;
        let chord = m.alt && m.shift && !others;
        let fresh_chord = chord && !self.dictation.chord_down;
        self.dictation.chord_down = chord;
        if alt_only || chord {
            if self.dictation.active() {
                if chord && self.dictation.by_key {
                    // ⇧ added while holding ⌥: keep going without holding anything.
                    self.dictation.by_key = false;
                    cx.notify();
                } else if fresh_chord && !self.dictation.by_key && self.dictation.alt_since.is_none() {
                    // ⇧⌥ again: that's the end of a hands-free take.
                    self.dictate_stop(window, cx);
                }
                self.dictation.alt_since.get_or_insert_with(Instant::now);
                return;
            }
            if chord {
                self.dictation.alt_hands_free = true;
            }
            let in_box = self.composer.focus_handle(cx).is_focused(window);
            if in_box && self.dictation.alt_since.is_none() {
                let at = Instant::now();
                self.dictation.alt_since = Some(at);
                self.dictation.alt_spoiled = false;
                self.dictation.alt_hands_free = chord;
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor().timer(HOLD).await;
                    let _ = in_window(&this, cx, |this, window, cx| {
                        let d = &this.dictation;
                        if d.alt_since == Some(at) && !d.alt_spoiled && !d.active() {
                            let hands_free = d.alt_hands_free;
                            this.dictate_start(!hands_free, window, cx);
                        }
                    });
                })
                .detach();
            }
            return;
        }
        if m.alt {
            // ⌥ with something else: a shortcut, not talking.
            self.dictation.alt_spoiled = true;
            return;
        }
        // ⌥ let go.
        self.dictation.alt_since = None;
        self.dictation.alt_hands_free = false;
        if self.dictation.by_key && self.dictation.active() {
            self.dictation.by_key = false;
            self.dictate_stop(window, cx);
        }
    }

    /// a key while ⌥ is down makes it a ⌥ combination; escape while listening throws the take away.
    pub(crate) fn dictate_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.dictation.alt_since.is_some() {
            self.dictation.alt_spoiled = true;
        }
        if key == "escape" && self.dictation.active() {
            self.dictate_cancel(window, cx);
            return true;
        }
        false
    }

    /// above what's typed while listening: the voice as a waveform, and the words so far.
    pub(crate) fn dictation_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let d = &self.dictation;
        if !d.active() {
            let p = self.palette.get();
            return d.tidying.as_ref().map(|_| {
                h_flex()
                    .id("dictation-tidying")
                    .test_support()
                    .pb(theme::z(6.))
                    .gap(theme::z(6.))
                    .set(Type::Subheadline)
                    .text_color(p.tertiary)
                    .child(gpui_kit::component::Icon::empty().path("icons/sparkles.svg").size(theme::z(12.)))
                    .child("Tidying what you said…")
                    .into_any_element()
            });
        }
        let p = self.palette.get();
        let levels: Vec<f32> = d.levels.iter().copied().collect();
        let tint = p.failed;
        let wave = canvas(
            |_, _, _| (),
            move |b, (), window, _| {
                let n = BARS as f32;
                let w = f32::from(b.size.width) / n;
                let h = f32::from(b.size.height);
                let pad = BARS.saturating_sub(levels.len());
                for i in 0..BARS {
                    let l = if i < pad { 0.0 } else { levels[i - pad] };
                    let bar = (2.0 + l * (h - 2.0)).min(h);
                    let x = f32::from(b.origin.x) + i as f32 * w + w * 0.2;
                    let y = f32::from(b.origin.y) + (h - bar) / 2.0;
                    window.paint_quad(fill(Bounds::new(point(px(x), px(y)), size(px(w * 0.6), px(bar))), tint.opacity(0.35 + 0.65 * l)).corner_radii(px(w * 0.3)));
                }
            },
        )
        .w(theme::z(72.))
        .h(theme::z(18.))
        .flex_shrink_0();
        let (line, quiet) = match &d.state {
            Listen::Waking => ("Starting to listen…".to_owned(), true),
            Listen::Preparing(what, progress) => (format!("{what}… {:.0}%", progress * 100.0), true),
            Listen::On if d.words.trim().is_empty() => ("Listening…".to_owned(), true),
            Listen::On => (tail(&d.words, 140), false),
            Listen::Finishing => (format!("{}…", tail(&d.words, 140)), false),
            Listen::Off => return None,
        };
        let hint = if d.by_key { "let go of ⌥ to finish · ⇧ to keep going · esc to throw away" } else { "⇧⌥ or Done to finish · esc to throw away" };
        let hands_free = !d.by_key && matches!(d.state, Listen::On | Listen::Waking | Listen::Preparing(..));
        Some(
            h_flex()
                .id("dictation")
                .test_support()
                .w_full()
                .pb(theme::z(8.))
                .gap(theme::z(10.))
                .items_center()
                .child(wave)
                .child(div().flex_1().min_w_0().set(Type::Callout).text_color(if quiet { p.tertiary } else { p.secondary }).truncate().child(line))
                .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(hint))
                .when(hands_free, |d| {
                    d.child(
                        Button::new("dictation-cancel")
                            .ghost()
                            .xsmall()
                            .icon(gpui_kit::component::Icon::empty().path("icons/x.svg"))
                            .tooltip("Throw it away (esc)")
                            .on_click(cx.listener(|this, _, window, cx| this.dictate_cancel(window, cx))),
                    )
                    .child(
                        Button::new("dictation-done")
                            .primary()
                            .xsmall()
                            .label("Done")
                            .tooltip("Put what you said in the box (⇧⌥)")
                            .on_click(cx.listener(|this, _, window, cx| this.dictate_stop(window, cx))),
                    )
                })
                .into_any_element(),
        )
    }

    /// the mic beside send: talking without holding a key.
    pub(crate) fn mic_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let on = self.dictation.active();
        Button::new("mic")
            .ghost()
            .small()
            .icon(gpui_kit::component::Icon::empty().path("icons/mic.svg").text_color(if on { p.failed } else { p.secondary }))
            .tooltip(if on { "Finish (or let go of ⌥)" } else { "Talk instead of typing (or hold ⌥)" })
            .on_click(cx.listener(|this, _, window, cx| this.dictate_toggle(window, cx)))
            .into_any_element()
    }
}

/// the first word without its capital, unless it's always written so ("I", "API").
fn lower_first(s: &str) -> String {
    let first = s.split_whitespace().next().unwrap_or_default();
    let shouty = first.chars().filter(|c| c.is_alphabetic()).count() > 1 && first.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase);
    if first == "I" || first.starts_with("I'") || shouty {
        return s.to_owned();
    }
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_lowercase().collect::<String>() + c.as_str())
}

/// the end of a long line, where the newest words are.
fn tail(s: &str, n: usize) -> String {
    let chars: Vec<char> = s.trim().chars().collect();
    if chars.len() <= n {
        return chars.into_iter().collect();
    }
    format!("…{}", chars[chars.len() - n..].iter().collect::<String>().trim_start())
}

#[cfg(test)]
mod tests {
    use super::{Heard, heard, tail};

    #[test]
    fn what_the_helper_says_is_read() {
        assert_eq!(heard(r#"{"ev":"partial","text":"hello wor"}"#), Some(Heard::Partial("hello wor".into())));
        assert_eq!(heard(r#"{"ev":"level","level":0.5}"#), Some(Heard::Level(0.5)));
        assert_eq!(heard(r#"{"ev":"preparing","what":"Downloading","progress":0.25}"#), Some(Heard::Preparing("Downloading".into(), 0.25)));
        assert_eq!(heard(r#"{"ev":"somethingnew"}"#), None, "what a newer helper says that this doesn't know is left alone");
        assert_eq!(heard("not json"), None);
    }

    #[test]
    fn carrying_on_a_sentence_keeps_its_words_lowercase() {
        assert_eq!(super::lower_first("And the login"), "and the login");
        assert_eq!(super::lower_first("I think so"), "I think so");
        assert_eq!(super::lower_first("API keys too"), "API keys too");
    }

    #[test]
    fn a_long_take_shows_its_newest_words() {
        assert_eq!(tail("short", 10), "short");
        assert_eq!(tail("one two three four", 9), "…hree four");
    }
}
