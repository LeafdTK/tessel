//! the browser: a page per session that the person watches and steers from a
//! pane and the agent drives with its tools. both act on the same page, so the
//! person sees what the agent does as it does it, and can point at a part of
//! the page to tell the agent about it. a window shows it natively and takes
//! the agent's acts relayed through the daemon; with no window open, the daemon
//! runs it in a browser of its own.

use serde::{Deserialize, Serialize};

/// something done to a session's page. the same list serves the pane (your
/// clicks and keys, the toolbar) and the agent's tools.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "act", rename_all = "snake_case")]
pub enum BrowserAct {
    /// make sure the session has a page; go to `url` if given (else, the first
    /// time, its workspace's `localhost:$TESSEL_PORT`).
    Open {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        url: Option<String>,
    },
    Navigate { url: String },
    Back,
    Forward,
    Reload,
    /// click an element (by CSS selector) or a point (in CSS pixels).
    Click {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
    },
    /// type text, into `selector` (clicked first) or wherever the focus is.
    Type {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selector: Option<String>,
    },
    /// a named key: Enter, Tab, Escape, Backspace, ArrowDown...
    Key { key: String },
    Scroll {
        #[serde(default)]
        dx: f64,
        #[serde(default)]
        dy: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        x: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        y: Option<f64>,
    },
    /// the page as text: its words, and what can be clicked or typed into, with selectors.
    Read,
    /// a picture of the page as it is now.
    Screenshot,
    // ---- the pane ----
    /// frames of the page to this connection, or not.
    Watch { on: bool },
    /// how big the pane is, in CSS pixels, and its screen's scale.
    Resize { width: u32, height: u32, scale: f64 },
    /// point at an element to show it to the agent: the next click picks.
    Pick { on: bool },
    /// the pointer, as the pane sees it.
    Mouse {
        kind: MouseKind,
        x: f64,
        y: f64,
        #[serde(default)]
        clicks: u32,
        #[serde(default)]
        modifiers: u32,
    },
    Wheel { x: f64, y: f64, dx: f64, dy: f64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MouseKind {
    Move,
    Down,
    Up,
}

/// where a session's page is.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserState {
    pub url: String,
    pub title: String,
    #[serde(default)]
    pub loading: bool,
    #[serde(default)]
    pub picking: bool,
}

/// an element the person pointed at, for the agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Picked {
    pub url: String,
    pub selector: String,
    /// its HTML, cut short when long.
    pub html: String,
    /// its text, cut short when long.
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// a PNG of it, base64.
    pub image: String,
}

/// the browser engine the daemon drives.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BrowserEngine {
    /// "Google Chrome", "Chrome for Testing"... none: there isn't one; it can be installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// being installed right now: what it's doing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installing: Option<String>,
}

/// a short CSS path to an element: its id if it has one, else tag and position up to one that does.
pub const CSS_PATH: &str = "function cssPath(el) {
  if (el.id) return '#' + CSS.escape(el.id);
  const parts = [];
  while (el && el.nodeType === 1 && el !== document.body && el !== document.documentElement) {
    let p = el.tagName.toLowerCase();
    const same = [...el.parentNode.children].filter(c => c.tagName === el.tagName);
    if (same.length > 1) p += ':nth-of-type(' + (same.indexOf(el) + 1) + ')';
    parts.unshift(p);
    el = el.parentNode;
    if (el && el.id) { parts.unshift('#' + CSS.escape(el.id)); break; }
  }
  return parts.join(' > ') || 'body';
}";

/// the page as text, for an agent: its words, then what it can use, with selectors.
pub fn read_script() -> String {
    format!(
        "(() => {{ {CSS_PATH}
  const shown = el => {{ const r = el.getBoundingClientRect(); const s = getComputedStyle(el); return r.width > 0 && r.height > 0 && s.visibility !== 'hidden' && s.display !== 'none'; }};
  const usable = [...document.querySelectorAll('a,button,input,textarea,select,summary,[role=button],[role=link],[contenteditable=true]')].filter(shown).slice(0, 150).map(el => {{
    const t = (el.innerText || el.value || el.placeholder || el.getAttribute('aria-label') || el.title || '').trim().replace(/\\s+/g, ' ').slice(0, 80);
    return '- ' + el.tagName.toLowerCase() + (el.type ? '[' + el.type + ']' : '') + ' \"' + t + '\" -> ' + cssPath(el);
  }});
  const text = (document.body ? document.body.innerText : '').replace(/\\n{{3,}}/g, '\\n\\n').slice(0, 8000);
  return document.title + '\\n' + location.href + '\\n\\n' + text + '\\n\\nwhat can be used:\\n' + usable.join('\\n');
}})()"
    )
}

/// where an element is, scrolled into view, and what it says.
pub fn locate_script(selector: &str) -> String {
    let sel = serde_json::to_string(selector).unwrap_or_default();
    format!(
        "(() => {{ const el = document.querySelector({sel}); if (!el) return null; el.scrollIntoView({{block: 'center', inline: 'center'}});
  const r = el.getBoundingClientRect(); return {{x: r.x + r.width / 2, y: r.y + r.height / 2, label: (el.innerText || el.value || el.getAttribute('aria-label') || el.tagName.toLowerCase()).trim().replace(/\\s+/g, ' ').slice(0, 40)}}; }})()"
    )
}

pub fn url_of(s: &str) -> String {
    let s = s.trim();
    if s.contains("://") || s.starts_with("about:") || s.starts_with("data:") {
        s.to_owned()
    } else if s.starts_with("localhost") || s.starts_with("127.0.0.1") {
        format!("http://{s}")
    } else {
        format!("https://{s}")
    }
}

/// a key's codes, for the keys an agent names.
pub fn key_codes(key: &str) -> (&'static str, &'static str, u32, Option<&'static str>) {
    match key.to_ascii_lowercase().as_str() {
        "enter" | "return" => ("Enter", "Enter", 13, Some("\r")),
        "tab" => ("Tab", "Tab", 9, None),
        "escape" | "esc" => ("Escape", "Escape", 27, None),
        "backspace" => ("Backspace", "Backspace", 8, None),
        "delete" => ("Delete", "Delete", 46, None),
        "arrowup" | "up" => ("ArrowUp", "ArrowUp", 38, None),
        "arrowdown" | "down" => ("ArrowDown", "ArrowDown", 40, None),
        "arrowleft" | "left" => ("ArrowLeft", "ArrowLeft", 37, None),
        "arrowright" | "right" => ("ArrowRight", "ArrowRight", 39, None),
        "home" => ("Home", "Home", 36, None),
        "end" => ("End", "End", 35, None),
        "pageup" => ("PageUp", "PageUp", 33, None),
        "pagedown" => ("PageDown", "PageDown", 34, None),
        "space" => (" ", "Space", 32, Some(" ")),
        _ => ("", "", 0, None),
    }
}
