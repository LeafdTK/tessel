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
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Picked {
    pub url: String,
    pub selector: String,
    /// its opening tag (attributes and all), cut short when long.
    pub html: String,
    /// its text, cut short when long.
    pub text: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    /// a PNG of it, base64.
    pub image: String,
    /// its tag: `button`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tag: String,
    /// what it's called: its label, alt text, placeholder or text, short.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// where in the source it was made, when the page says (a development
    /// build of React, Vue or Svelte does): `src/Login.tsx:42`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub source: String,
    /// the components it's in, innermost first: `Button > LoginForm`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub components: String,
    /// the styles that make how it looks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub styles: String,
    /// what it's inside, a few levels up.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub inside: String,
}

impl Picked {
    /// from what `DESCRIBE`'s `describeEl` answered, and a picture of it.
    pub fn from_described(v: &serde_json::Value, image: String) -> Picked {
        let s = |k: &str| v[k].as_str().unwrap_or_default().to_owned();
        let n = |k: &str| v[k].as_f64().unwrap_or(0.0);
        Picked {
            url: s("url"),
            selector: s("selector"),
            html: s("html"),
            text: s("text"),
            x: n("x"),
            y: n("y"),
            width: n("width"),
            height: n("height"),
            image,
            tag: s("tag"),
            name: s("name"),
            source: s("source"),
            components: s("components"),
            styles: s("styles"),
            inside: s("inside"),
        }
    }

    /// what it's called on its chip: `button "Sign in"`.
    pub fn label(&self) -> String {
        let tag = if self.tag.is_empty() { "element" } else { self.tag.as_str() };
        let name: String = self.name.chars().take(32).collect();
        if name.is_empty() { tag.to_owned() } else { format!("{tag} \"{name}\"") }
    }

    /// what the agent reads about it, beside its picture: enough to find it
    /// in the page and in the code, in a few lines.
    pub fn note(&self) -> String {
        let mut lines = vec![format!("page: {}", self.url), format!("element: {}", self.html)];
        if !self.name.is_empty() {
            lines.push(format!("called: {}", self.name));
        }
        lines.push(format!("selector: {}", self.selector));
        match (self.source.is_empty(), self.components.is_empty()) {
            (false, false) => lines.push(format!("source: {} (in {})", self.source, self.components)),
            (false, true) => lines.push(format!("source: {}", self.source)),
            (true, false) => lines.push(format!("components: {}", self.components)),
            (true, true) => {}
        }
        if !self.inside.is_empty() {
            lines.push(format!("inside: {}", self.inside));
        }
        lines.push(format!("box: {:.0}x{:.0} at {:.0},{:.0}", self.width, self.height, self.x, self.y));
        if !self.styles.is_empty() {
            lines.push(format!("style: {}", self.styles));
        }
        format!("<picked_element>\nThe person pointed at this in the browser; its picture comes next.\n{}\n</picked_element>", lines.join("\n"))
    }
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

/// `describeEl(el)`: what an element is, where (on the page and, in a
/// development build, in the source) and how it looks, in a few short fields.
/// needs `CSS_PATH` beside it.
pub const DESCRIBE: &str = r#"function describeEl(el) {
  const r = el.getBoundingClientRect();
  const short = (s, n) => String(s || '').trim().replace(/\s+/g, ' ').slice(0, n);
  const html = el.outerHTML.slice(0, el.outerHTML.indexOf('>') + 1).slice(0, 300);
  const name = short(el.getAttribute('aria-label') || el.getAttribute('alt') || el.getAttribute('title') || el.getAttribute('placeholder') || el.innerText || el.value, 80);
  const inside = [];
  for (let p = el.parentElement; p && p !== document.body && p !== document.documentElement && inside.length < 4; p = p.parentElement) {
    const cls = typeof p.className === 'string' && p.className.trim() ? '.' + p.className.trim().split(/\s+/).slice(0, 2).join('.') : '';
    inside.unshift(p.tagName.toLowerCase() + (p.id ? '#' + p.id : '') + cls);
  }
  const s = getComputedStyle(el), st = [];
  st.push('font ' + s.fontSize + ' ' + s.fontWeight + ' ' + s.fontFamily.split(',')[0].replace(/["']/g, ''));
  st.push('color ' + s.color + (s.backgroundColor !== 'rgba(0, 0, 0, 0)' ? ' on ' + s.backgroundColor : ''));
  if (s.padding && s.padding !== '0px') st.push('padding ' + s.padding);
  if (s.margin && s.margin !== '0px') st.push('margin ' + s.margin);
  if (s.borderRadius && s.borderRadius !== '0px') st.push('radius ' + s.borderRadius);
  if (s.borderStyle && s.borderStyle !== 'none' && s.borderWidth !== '0px') st.push('border ' + s.borderWidth + ' ' + s.borderStyle + ' ' + s.borderColor);
  st.push('display ' + s.display + (s.gap && s.gap !== 'normal' ? ', gap ' + s.gap : ''));
  const clean = f => String(f || '').replace(/^webpack:\/\/[^/]*\//, '').replace(/^\/@fs/, '').replace(location.origin, '').replace(/\?.*$/, '');
  let source = '';
  const comps = [];
  const fiber = Object.keys(el).find(k => k.startsWith('__reactFiber$') || k.startsWith('__reactInternalInstance$'));
  for (let f = fiber && el[fiber]; f && comps.length < 4; f = f.return) {
    const t = f.type;
    const nm = t && typeof t !== 'string' && (t.displayName || t.name || (t.render && (t.render.displayName || t.render.name)));
    if (nm && !comps.includes(nm)) comps.push(nm);
    if (!source && f._debugSource) source = clean(f._debugSource.fileName) + ':' + f._debugSource.lineNumber;
  }
  for (let v = el.__vueParentComponent; v && comps.length < 4; v = v.parent) {
    const t = v.type || {};
    const nm = t.name || t.__name;
    if (nm && !comps.includes(nm)) comps.push(nm);
    if (!source && t.__file) source = clean(t.__file);
  }
  const sv = el.__svelte_meta && el.__svelte_meta.loc;
  if (!source && sv) source = clean(sv.file) + ':' + sv.line;
  const tagged = el.closest('[data-source],[data-loc],[data-source-file],[data-inspector-relative-path]');
  if (!source && tagged) {
    const a = k => tagged.getAttribute(k);
    source = a('data-source') || a('data-loc') || a('data-source-file') || (a('data-inspector-relative-path') + (a('data-inspector-line') ? ':' + a('data-inspector-line') : ''));
  }
  return {selector: cssPath(el), tag: el.tagName.toLowerCase(), name, text: short(el.innerText, 300), html,
    x: r.x + scrollX, y: r.y + scrollY, vx: r.x, vy: r.y, width: r.width, height: r.height, url: location.href,
    source, components: comps.join(' > '), styles: st.join('; '), inside: inside.join(' > ')};
}"#;
