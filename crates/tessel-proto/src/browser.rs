//! the browser: a page per session, run by the daemon, that the person watches
//! and steers from a pane and the agent drives with its tools. both act on the
//! same page, so the person sees what the agent does as it does it, and can
//! point at a part of the page to tell the agent about it.

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
