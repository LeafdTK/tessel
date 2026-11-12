//! the window as data.
//!
//! everything you see is a tree of components read from `ui.json` in the
//! daemon's home (`~/.tessel/ui.json`). the file is watched: save it and the
//! window rebuilds itself. a person can write it, and so can an agent: the
//! vocabulary is small, every mistake is reported with where it is and what
//! would have been accepted, and a broken file never takes the window down
//! (the last good one stays up, with the error shown over it).
//!
//! this module only reads the file. `shell/compose.rs` draws what it describes.

use crate::theme::{Palette, Type};
use gpui_kit::{FontWeight, Hsla, Rgba};
use serde_json::{Map, Value};
use std::fmt::Write as _;

pub mod harmony;
pub mod themes;

pub const REFERENCE: &str = include_str!("README.md");

/// the layouts tessel ships with. a `ui.json` can start from one and change parts.
pub const PRESETS: [(&str, &str); 4] = [
    ("default", include_str!("presets/default.json")),
    ("focus", include_str!("presets/focus.json")),
    ("tiles", include_str!("presets/tiles.json")),
    ("rail", include_str!("presets/rail.json")),
];

/// the layouts offered by name, as people see them: name, title, what it is.
pub const LAYOUTS: [(&str, &str, &str); 4] = [
    ("default", "Studio", "The conversation, with the workspace beside it"),
    ("focus", "Focus", "Only the conversation"),
    ("tiles", "Tiles", "The conversation, a terminal and git"),
    ("rail", "Rail", "Sessions as a pane beside the conversation"),
];

pub fn preset(name: &str) -> Option<&'static str> {
    PRESETS.iter().find(|(n, _)| *n == name).map(|(_, text)| *text)
}

/// a whole window: what to draw, and in what colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Ui {
    pub root: Node,
    pub theme: ThemeSpec,
    /// drawn exactly as written: no scales, no contrast fixes, no warnings.
    pub freestyle: bool,
    /// the agents at work, drawn around the camera notch. none: on where
    /// there is a notch, off where there isn't.
    pub notch: Option<bool>,
    /// every desktop, in order. empty: the file is one desktop.
    pub desktops: Vec<Desktop>,
    /// which of them is showing (`root` is its layout).
    pub desktop: usize,
    /// what was adjusted to fit tessel's scales, and what still reads badly.
    pub report: harmony::Report,
}

impl Ui {
    /// the layout used when there is no `ui.json`, or one that only sets a theme.
    pub fn default_layout() -> Self {
        parse(preset("default").expect("the default is shipped")).expect("the default layout is valid")
    }

    /// every node, depth first.
    pub fn nodes(&self) -> Vec<&Node> {
        let mut out = Vec::new();
        self.root.walk(&mut |n| out.push(n));
        out
    }

    /// the first node of a kind, if the layout has one.
    pub fn find(&self, kind: Kind) -> Option<&Node> {
        self.nodes().into_iter().find(|n| n.kind == kind)
    }

    /// every pane in every `panes` node, in order.
    pub fn panes(&self) -> Vec<&Pane> {
        let mut out = Vec::new();
        for n in self.nodes() {
            if let Some(tree) = &n.props.layout {
                tree.leaves(&mut out);
            }
        }
        out
    }

    pub fn has_module(&self, m: Module) -> bool {
        self.panes().iter().any(|p| p.module == Some(m))
    }

    /// the drawer a kind of node sits in, if any.
    pub fn drawer_holding(&self, kind: Kind) -> Option<String> {
        fn search(n: &Node, kind: Kind, drawer: Option<&str>) -> Option<Option<String>> {
            let here = if n.kind == Kind::Drawer { n.id.as_deref() } else { drawer };
            if n.kind == kind {
                return Some(here.map(str::to_owned));
            }
            n.children.iter().find_map(|c| search(c, kind, here))
        }
        search(&self.root, kind, None).flatten()
    }
}

/// one desktop: a layout of its own, the projects its sidebar lists, and the
/// chat it had open. sessions don't belong to desktops; desktops show them.
#[derive(Clone, Debug, PartialEq)]
pub struct Desktop {
    /// stable across renames: what the window remembers a desktop's state by.
    pub id: String,
    pub name: String,
    /// project names the sidebar lists here. empty: every project.
    pub scope: Vec<String>,
    /// the chat it had open, by session id.
    pub open: Option<String>,
}

// ---- parsing -------------------------------------------------------------------

/// read a `ui.json`. errors name the place in the file and what would have been accepted.
pub fn parse(text: &str) -> Result<Ui, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("ui.json is not valid JSON: {e}"))?;
    let (v, desktops, desktop) = desktop_overlay(v)?;
    let mut ui = parse_value(&v, 0)?;
    ui.desktops = desktops;
    ui.desktop = desktop;
    Ok(ui)
}

/// the first `panes` node's `layout` in a layout tree.
pub fn panes_layout(v: &Value) -> Option<&Value> {
    if v.get("type").and_then(Value::as_str) == Some("panes") {
        return v.get("layout");
    }
    v.get("children")?.as_array()?.iter().find_map(panes_layout)
}

pub fn panes_layout_mut(v: &mut Value) -> Option<&mut Value> {
    if v.get("type").and_then(Value::as_str) == Some("panes") {
        return v.get_mut("layout");
    }
    v.get_mut("children")?.as_array_mut()?.iter_mut().find_map(panes_layout_mut)
}

/// a desktop's panes, from what its entry holds: its own `layout`, or (as
/// entries once were) a whole `root` or `preset`, of which only the panes count.
pub fn desktop_layout(entry: &Map<String, Value>) -> Option<Value> {
    if let Some(l) = entry.get("layout") {
        return Some(l.clone());
    }
    let root = match (entry.get("root"), entry.get("preset").and_then(Value::as_str)) {
        (Some(root), _) => root.clone(),
        (None, Some(name)) => serde_json::from_str::<Value>(preset(name)?).ok()?.get("root")?.clone(),
        _ => return None,
    };
    panes_layout(&root).cloned()
}

/// a file with `desktops` is read as the desktop that's showing: its panes
/// in place of the file's, and everything else (the sidebar, the theme, the
/// options) shared by every desktop.
fn desktop_overlay(v: Value) -> Result<(Value, Vec<Desktop>, usize), String> {
    let Value::Object(mut obj) = v else { return Ok((v, Vec::new(), 0)) };
    let list = match obj.remove("desktops") {
        None => {
            obj.remove("desktop");
            return Ok((Value::Object(obj), Vec::new(), 0));
        }
        Some(Value::Array(list)) if !list.is_empty() => list,
        Some(_) => return Err("desktops: expected a list of desktops, like [{\"name\": \"Orchard\"}]".into()),
    };
    let mut desktops = Vec::new();
    let mut layouts = Vec::new();
    for (i, d) in list.iter().enumerate() {
        let at = format!("desktops[{i}]");
        let o = d.as_object().ok_or_else(|| format!("{at}: expected an object with a \"name\""))?;
        keys(o, &at, &["id", "name", "layout", "root", "preset", "scope", "open"])?;
        let name = o.get("name").map(|n| text(n, &format!("{at}.name"))).transpose()?.map(str::to_owned).unwrap_or_else(|| format!("Desktop {}", i + 1));
        let id = o.get("id").map(|n| text(n, &format!("{at}.id"))).transpose()?.map(str::to_owned).unwrap_or_else(|| format!("d{}", i + 1));
        if desktops.iter().any(|d: &Desktop| d.id == id) {
            return Err(format!("{at}.id: \"{id}\" is used twice"));
        }
        let scope = match o.get("scope") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.iter().map(|p| text(p, &format!("{at}.scope")).map(str::to_owned)).collect::<Result<_, _>>()?,
            Some(_) => return Err(format!("{at}.scope: expected a list of project names, like [\"orchard\"]")),
        };
        let open = o.get("open").map(|n| text(n, &format!("{at}.open"))).transpose()?.map(str::to_owned);
        if let Some(p) = o.get("preset").and_then(Value::as_str)
            && preset(p).is_none()
        {
            return Err(format!("{at}.preset: there is no preset \"{p}\". presets: {}", preset_names()));
        }
        layouts.push(desktop_layout(o));
        desktops.push(Desktop { id, name, scope, open });
    }
    let showing = match obj.remove("desktop") {
        None => 0,
        Some(Value::String(id)) => desktops.iter().position(|d| d.id == id).unwrap_or(0),
        Some(Value::Number(n)) => (n.as_u64().unwrap_or(1).max(1) as usize - 1).min(desktops.len() - 1),
        Some(_) => return Err("desktop: expected a desktop's id, like \"d2\"".into()),
    };
    if let Some(layout) = layouts.swap_remove(showing) {
        // the shared layout, written out, with this desktop's panes in it.
        let mut root = match (obj.get("root"), obj.get("preset").and_then(Value::as_str)) {
            (Some(r), _) => r.clone(),
            (None, name) => preset(name.unwrap_or("default")).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("root").cloned()).unwrap_or(Value::Null),
        };
        if let Some(slot) = panes_layout_mut(&mut root) {
            *slot = layout;
            obj.insert("root".into(), root);
        }
    }
    Ok((Value::Object(obj), desktops, showing))
}

fn parse_value(v: &Value, depth: usize) -> Result<Ui, String> {
    let obj = v.as_object().ok_or("ui.json must be an object with \"root\" and/or \"preset\"")?;
    keys(obj, "ui.json", &["$schema", "preset", "theme", "root", "freestyle", "transcript", "sessions", "notch"])?;
    let freestyle = obj.get("freestyle").map(|v| flag(v, "freestyle")).transpose()?.unwrap_or(false);
    let notch = obj.get("notch").map(|v| flag(v, "notch")).transpose()?;
    let base = match obj.get("preset") {
        None => None,
        Some(Value::String(name)) if depth < 4 => {
            let text = preset(name).ok_or_else(|| format!("preset: there is no preset \"{name}\". presets: {}", preset_names()))?;
            let v: Value = serde_json::from_str(text).map_err(|e| format!("preset {name} is broken: {e}"))?;
            Some(parse_value(&v, depth + 1)?)
        }
        Some(_) => return Err(format!("preset: expected one of {}", preset_names())),
    };
    let base = match base {
        Some(b) => Some(b),
        // a file with no layout of its own (only a theme, say) uses the default one.
        None if obj.get("root").is_none() && depth == 0 => Some(Ui::default_layout()),
        None => None,
    };
    let root = match (obj.get("root"), &base) {
        (Some(r), _) => Node::parse(r, "root")?,
        (None, Some(b)) => b.root.clone(),
        (None, None) => return Err("a preset needs a \"root\" node".into()),
    };
    let base_notch = base.as_ref().map(|b| b.notch);
    let mut theme = base.map(|b| b.theme).unwrap_or_default();
    if let Some(t) = obj.get("theme") {
        theme.merge(ThemeSpec::parse(t)?);
    }
    theme.freestyle = freestyle;
    let mut root = root;
    // shorthands for the options people change most, wherever the layout put
    // the component: the settings page writes these.
    for (key, kind, allowed) in [("transcript", Kind::Transcript, &["user", "thoughts", "tools"][..]), ("sessions", Kind::Sessions, &["trace"][..])] {
        let Some(v) = obj.get(key) else { continue };
        let o = v.as_object().ok_or_else(|| format!("{key}: expected an object, like {{\"{}\": ...}}", allowed[0]))?;
        keys(o, key, allowed)?;
        let given = Props::parse(kind, o, key)?;
        let mut found = false;
        let module = if kind == Kind::Transcript { Module::Conversation } else { Module::Sessions };
        root.walk_mut(&mut |n| {
            if let Some(tree) = &mut n.props.layout {
                tree.each_mut(&mut |pane| {
                    if pane.module == Some(module) {
                        found = true;
                        for k in o.keys() {
                            match k.as_str() {
                                "user" => pane.options.user_left = given.user_left,
                                "thoughts" => pane.options.thoughts = given.thoughts,
                                "tools" => pane.options.tools = given.tools,
                                "trace" => pane.options.trace = given.trace,
                                _ => {}
                            }
                        }
                    }
                });
            }
            if n.kind == kind {
                found = true;
                for k in o.keys() {
                    match k.as_str() {
                        "user" => n.props.user_left = given.user_left,
                        "thoughts" => n.props.thoughts = given.thoughts,
                        "tools" => n.props.tools = given.tools,
                        "trace" => n.props.trace = given.trace,
                        _ => {}
                    }
                }
            }
        });
        if !found {
            return Err(format!("{key}: this layout has no \"{}\" for these options to apply to", kind.name()));
        }
    }
    let notch = notch.or(base_notch.flatten());
    if depth == 0 {
        place_desktops(&mut root);
    }
    let mut ui = Ui { root, theme, freestyle, notch, desktops: Vec::new(), desktop: 0, report: harmony::Report::default() };
    check(&ui)?;
    ui.report = harmony::harmonize(&mut ui);
    Ok(ui)
}

/// a layout that doesn't say where the desktops go gets them beside its
/// settings button, so a layout made before desktops existed shows them too.
fn place_desktops(root: &mut Node) {
    fn has(n: &Node) -> bool {
        n.kind == Kind::Desktops || n.children.iter().any(has)
    }
    fn place(n: &mut Node) -> bool {
        if let Some(i) = n.children.iter().position(|c| c.id.as_deref() == Some("settings")) {
            if let Ok(d) = Node::parse(&serde_json::json!({"type": "desktops"}), &format!("{}.desktops", n.path)) {
                n.children.insert(i + 1, d);
            }
            return true;
        }
        n.children.iter_mut().any(place)
    }
    if !has(root) {
        place(root);
    }
}

fn preset_names() -> String {
    PRESETS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

/// what can only be checked once the whole tree is known.
fn check(ui: &Ui) -> Result<(), String> {
    let nodes = ui.nodes();
    let mut drawers: Vec<&str> = nodes.iter().filter(|n| n.kind == Kind::Drawer).filter_map(|n| n.id.as_deref()).collect();
    // panes open and close like drawers, by id.
    let panes = ui.panes();
    drawers.extend(panes.iter().map(|p| p.id.as_str()));
    for (i, d) in drawers.iter().enumerate() {
        if drawers[..i].contains(d) {
            return Err(format!("two drawers or panes are called \"{d}\". ids must be different"));
        }
    }
    let known = |id: &str| drawers.contains(&id);
    let list = || if drawers.is_empty() { "there are no drawers or panes".to_owned() } else { format!("drawers and panes: {}", drawers.join(", ")) };
    for n in &nodes {
        if let Some(Action::Toggle(id)) = &n.props.action
            && !known(id)
        {
            return Err(format!("{}: action \"toggle:{id}\" names a drawer that doesn't exist ({})", n.path, list()));
        }
        for c in &n.when {
            if let Cond::Open(id) = c.inner()
                && !known(id)
            {
                return Err(format!("{}: when \"open:{id}\" names a drawer that doesn't exist ({})", n.path, list()));
            }
        }
        if let Some(Inset::Drawer(id)) = &n.style.inset
            && !known(id)
        {
            return Err(format!("{}: inset \"{id}\" names a drawer that doesn't exist ({})", n.path, list()));
        }
    }
    // the conversation module holds a transcript, a message box and the new-session form.
    // conversation panes come in any number (each holds a chat), but a loose
    // transcript, input or new-session node is the one chat there is.
    let conversations = panes.iter().filter(|p| p.module == Some(Module::Conversation)).count();
    for kind in [Kind::Transcript, Kind::Input, Kind::Sessions, Kind::Panel, Kind::NewSession] {
        let mut n = nodes.iter().filter(|n| n.kind == kind).count();
        if matches!(kind, Kind::Transcript | Kind::Input | Kind::NewSession) && n > 0 {
            n += conversations;
        }
        if kind == Kind::Sessions {
            n += panes.iter().filter(|p| p.module == Some(Module::Sessions)).count();
        }
        if kind == Kind::Panel {
            n += panes.iter().filter(|p| p.module == Some(Module::Workspace)).count();
        }
        if n > 1 {
            let what = match kind {
                Kind::Transcript | Kind::Input | Kind::NewSession => "conversation (or transcript, input, new-session)",
                Kind::Sessions => "sessions",
                _ => "workspace (or panel)",
            };
            return Err(format!("there are {n} of {what}. there can be one"));
        }
    }
    Ok(())
}

/// refuse keys that mean nothing here, saying which would have.
fn keys(obj: &Map<String, Value>, at: &str, allowed: &[&str]) -> Result<(), String> {
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            let near = allowed.iter().find(|a| close(a, k)).map(|a| format!(" (did you mean \"{a}\"?)")).unwrap_or_default();
            return Err(format!("{at}: \"{k}\" is not something this can have{near}. it can have: {}", allowed.join(", ")));
        }
    }
    Ok(())
}

/// a likely typo: a prefix of the other, or a couple of letters off.
fn close(a: &str, b: &str) -> bool {
    let (a, b) = (a.to_ascii_lowercase().replace(['-', '_'], ""), b.to_ascii_lowercase().replace(['-', '_'], ""));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b || a.starts_with(&b) || b.starts_with(&a) {
        return true;
    }
    let allowed = if a.len().min(b.len()) >= 5 { 2 } else { 1 };
    edits(&a, &b) <= allowed
}

/// levenshtein distance.
fn edits(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut prev = row[0];
        row[0] = i + 1;
        for j in 0..b.len() {
            let cur = row[j + 1];
            row[j + 1] = (prev + usize::from(ca != b[j])).min(row[j] + 1).min(cur + 1);
            prev = cur;
        }
    }
    row[b.len()]
}

fn num(v: &Value, at: &str) -> Result<f32, String> {
    v.as_f64().map(|n| n as f32).ok_or_else(|| format!("{at}: expected a number, got {v}"))
}

fn text<'a>(v: &'a Value, at: &str) -> Result<&'a str, String> {
    v.as_str().ok_or_else(|| format!("{at}: expected a string, got {v}"))
}

fn flag(v: &Value, at: &str) -> Result<bool, String> {
    v.as_bool().ok_or_else(|| format!("{at}: expected true or false, got {v}"))
}

fn one_of<T: Copy>(v: &Value, at: &str, options: &[(&str, T)]) -> Result<T, String> {
    let s = text(v, at)?;
    options.iter().find(|(n, _)| *n == s).map(|(_, t)| *t).ok_or_else(|| {
        let names: Vec<&str> = options.iter().map(|(n, _)| *n).collect();
        format!("{at}: \"{s}\" isn't one of: {}", names.join(", "))
    })
}

// ---- the theme ----------------------------------------------------------------------

/// colour roles, named as in orchard's design system. a theme sets any of
/// them; the rest come from its base.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Token {
    /// the frame around the content: the sidebar.
    App,
    /// the content sheet.
    Sheet,
    /// surfaces that float above the sheet: cards, the composer.
    Raised,
    /// quiet fills set into the sheet: your messages, fields.
    Sunken,
    Hover,
    /// the selected row.
    Active,
    Text,
    Muted,
    Faint,
    /// the faintest ink: drag handles, inactive marks.
    Ghost,
    Border,
    Accent,
    OnAccent,
    /// blocked on you.
    Warning,
    Error,
    /// finished and not yet seen.
    Success,
}

pub const TOKENS: [(&str, Token); 16] = [
    ("app", Token::App),
    ("sheet", Token::Sheet),
    ("raised", Token::Raised),
    ("sunken", Token::Sunken),
    ("hover", Token::Hover),
    ("active", Token::Active),
    ("text", Token::Text),
    ("muted", Token::Muted),
    ("faint", Token::Faint),
    ("ghost", Token::Ghost),
    ("border", Token::Border),
    ("accent", Token::Accent),
    ("on-accent", Token::OnAccent),
    ("warning", Token::Warning),
    ("error", Token::Error),
    ("success", Token::Success),
];

/// what's behind the panes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Backdrop {
    #[default]
    None,
    /// a dithered band in the project's colours.
    Dither,
    /// a photograph from NASA's image library.
    Space,
}

impl Backdrop {
    pub fn name(self) -> &'static str {
        match self {
            Backdrop::None => "none",
            Backdrop::Dither => "dither",
            Backdrop::Space => "space",
        }
    }
}

/// which built-in palette a theme starts from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Base {
    /// apple's system colours. the default.
    #[default]
    Tessel,
    /// hack club orchard's default palette.
    Orchard,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThemeSpec {
    pub base: Option<Base>,
    /// `Some(true)` always dark, `Some(false)` always light, `None` follows the system.
    pub dark: Option<Option<bool>>,
    pub font: Option<String>,
    pub mono: Option<String>,
    pub light: Vec<(Token, Hsla)>,
    pub dark_tokens: Vec<(Token, Hsla)>,
    /// leave the colours exactly as given, readable or not.
    pub freestyle: bool,
    /// how big the interface is drawn: 1 is the default.
    pub scale: Option<f32>,
    /// panes flush with hairlines between them, instead of floating islands.
    pub compact: Option<bool>,
    /// the desktop shows through, blurred, behind the frame.
    pub glass: Option<bool>,
    /// the frame is felt: a fine grain with a soft sheen, the panes set into it.
    pub felt: Option<bool>,
    /// what's behind the panes: nothing unless asked for.
    pub backdrop: Option<Backdrop>,
    /// the terminal's text size, in points (it uses the code font).
    pub term_size: Option<f32>,
    /// the terminal's colours: theme (the window's), ink, paper or midnight.
    pub term_colors: Option<String>,
}

impl ThemeSpec {
    pub(crate) fn parse(v: &Value) -> Result<Self, String> {
        // "photon" is tessel's old name: a layout written then still reads.
        // "photon" is tessel's old name, and "tern" a palette that's gone: both still read, as tessel.
        let bases = [("tessel", Base::Tessel), ("orchard", Base::Orchard), ("photon", Base::Tessel), ("tern", Base::Tessel)];
        if v.is_string() {
            return Ok(Self { base: Some(one_of(v, "theme", &bases)?), ..Self::default() });
        }
        let obj = v.as_object().ok_or("theme: expected a base name (\"tessel\", \"orchard\") or an object")?;
        keys(obj, "theme", &["base", "appearance", "font", "mono", "light", "dark", "size", "density", "material", "backdrop", "terminal"])?;
        let mut t = Self::default();
        let (mut t_size, mut t_colors) = (None, None);
        if let Some(b) = obj.get("base") {
            t.base = Some(one_of(b, "theme.base", &bases)?);
        }
        if let Some(a) = obj.get("appearance") {
            t.dark = Some(one_of(a, "theme.appearance", &[("system", None), ("light", Some(false)), ("dark", Some(true))])?);
        }
        if let Some(f) = obj.get("font") {
            t.font = Some(text(f, "theme.font")?.to_owned());
        }
        if let Some(v) = obj.get("size") {
            t.scale = Some(match v {
                Value::String(_) => one_of(v, "theme.size", &crate::theme::SCALES)?,
                _ => {
                    let n = num(v, "theme.size")?;
                    if !(0.75..=1.5).contains(&n) {
                        return Err(format!("theme.size: {n} is out of range. use \"compact\", \"default\", \"large\", or a number from 0.75 to 1.5"));
                    }
                    n
                }
            });
        }
        if let Some(f) = obj.get("mono") {
            t.mono = Some(text(f, "theme.mono")?.to_owned());
        }
        if let Some(d) = obj.get("density") {
            t.compact = Some(one_of(d, "theme.density", &[("islands", false), ("compact", true)])?);
        }
        if let Some(m) = obj.get("material") {
            let (glass, felt) = one_of(m, "theme.material", &[("solid", (false, false)), ("glass", (true, false)), ("felt", (false, true))])?;
            t.glass = Some(glass);
            t.felt = Some(felt);
        }
        if let Some(t) = obj.get("terminal") {
            let o = t.as_object().ok_or("theme.terminal: expected an object, like {\"size\": 13, \"colors\": \"ink\"}")?;
            keys(o, "theme.terminal", &["size", "colors"])?;
            if let Some(v) = o.get("size") {
                let n = num(v, "theme.terminal.size")?;
                if !(9.0..=24.0).contains(&n) {
                    return Err(format!("theme.terminal.size: {n} is out of range: 9 to 24"));
                }
                t_size = Some(n);
            }
            if let Some(v) = o.get("colors") {
                t_colors = Some(one_of(v, "theme.terminal.colors", &[("theme", "theme"), ("ink", "ink"), ("paper", "paper"), ("midnight", "midnight")])?.to_owned());
            }
        }
        if let Some(b) = obj.get("backdrop") {
            t.backdrop = Some(one_of(b, "theme.backdrop", &[("none", Backdrop::None), ("dither", Backdrop::Dither), ("space", Backdrop::Space)])?);
        }
        t.term_size = t_size;
        t.term_colors = t_colors;
        for (key, out) in [("light", &mut t.light), ("dark", &mut t.dark_tokens)] {
            let Some(m) = obj.get(key) else { continue };
            let m = m.as_object().ok_or_else(|| format!("theme.{key}: expected an object of colour roles, like {{\"accent\": \"#df3047\"}}"))?;
            let names: Vec<&str> = TOKENS.iter().map(|(n, _)| *n).collect();
            keys(m, &format!("theme.{key}"), &names)?;
            for (name, value) in m {
                let token = TOKENS.iter().find(|(n, _)| n == name).map(|(_, t)| *t).expect("checked above");
                let at = format!("theme.{key}.{name}");
                out.push((token, hex(text(value, &at)?).ok_or_else(|| format!("{at}: \"{value}\" isn't a colour. write #rgb, #rrggbb or #rrggbbaa"))?));
            }
        }
        Ok(t)
    }

    /// layer another theme over this one: what it sets wins.
    pub fn merge(&mut self, over: ThemeSpec) {
        self.base = over.base.or(self.base);
        self.dark = over.dark.or(self.dark);
        self.font = over.font.or(self.font.take());
        self.scale = over.scale.or(self.scale);
        self.mono = over.mono.or(self.mono.take());
        self.compact = over.compact.or(self.compact);
        self.glass = over.glass.or(self.glass);
        self.felt = over.felt.or(self.felt);
        self.backdrop = over.backdrop.or(self.backdrop);
        self.term_size = over.term_size.or(self.term_size);
        self.term_colors = over.term_colors.or(self.term_colors.take());
        self.light.extend(over.light);
        self.dark_tokens.extend(over.dark_tokens);
    }

    /// the palette for one appearance.
    pub fn palette(&self, dark: bool) -> Palette {
        let mut p = match (self.base.unwrap_or_default(), dark) {
            (Base::Tessel, false) => Palette::light(),
            (Base::Tessel, true) => Palette::dark(),
            (Base::Orchard, false) => Palette::orchard_light(),
            (Base::Orchard, true) => Palette::orchard_dark(),
        };
        for (token, color) in if dark { &self.dark_tokens } else { &self.light } {
            p.set(*token, *color);
        }
        if !self.freestyle {
            p.keep_readable();
        }
        p
    }
}

/// `#rgb`, `#rrggbb` or `#rrggbbaa`.
pub fn hex(s: &str) -> Option<Hsla> {
    let h = s.strip_prefix('#')?;
    let digits: Vec<u8> = h.chars().map(|c| c.to_digit(16).map(|d| d as u8)).collect::<Option<_>>()?;
    let (r, g, b, a) = match digits.len() {
        3 => (digits[0] * 17, digits[1] * 17, digits[2] * 17, 255),
        6 => (digits[0] * 16 + digits[1], digits[2] * 16 + digits[3], digits[4] * 16 + digits[5], 255),
        8 => (digits[0] * 16 + digits[1], digits[2] * 16 + digits[3], digits[4] * 16 + digits[5], digits[6] * 16 + digits[7]),
        _ => return None,
    };
    let f = |v: u8| f32::from(v) / 255.0;
    Some(Rgba { r: f(r), g: f(g), b: f(b), a: f(a) }.into())
}

// ---- nodes ----------------------------------------------------------------------------

/// every kind of component a layout can use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    // arranging
    Row,
    Column,
    Drawer,
    Spacer,
    Divider,
    // small things
    Text,
    Button,
    Title,
    Status,
    Connection,
    Error,
    // the session
    Sessions,
    NewSession,
    Transcript,
    Prompts,
    // the message box, in pieces
    Attachments,
    Input,
    ModelPicker,
    ModePicker,
    Context,
    // beside the session
    Panel,
    // a tiled workspace of panes, each showing a module
    Panes,
    /// the + menu: open a folder, clone a repository, start a project, or a recent folder.
    Add,
    /// the desktops, by name: click one to go there, + for another.
    Desktops,
}

pub const KINDS: [(&str, Kind); 24] = [
    ("row", Kind::Row),
    ("column", Kind::Column),
    ("drawer", Kind::Drawer),
    ("spacer", Kind::Spacer),
    ("divider", Kind::Divider),
    ("text", Kind::Text),
    ("button", Kind::Button),
    ("title", Kind::Title),
    ("status", Kind::Status),
    ("connection", Kind::Connection),
    ("error", Kind::Error),
    ("sessions", Kind::Sessions),
    ("new-session", Kind::NewSession),
    ("transcript", Kind::Transcript),
    ("prompts", Kind::Prompts),
    ("attachments", Kind::Attachments),
    ("input", Kind::Input),
    ("model-picker", Kind::ModelPicker),
    ("mode-picker", Kind::ModePicker),
    ("context", Kind::Context),
    ("panel", Kind::Panel),
    ("panes", Kind::Panes),
    ("add", Kind::Add),
    ("desktops", Kind::Desktops),
];

impl Kind {
    pub fn name(self) -> &'static str {
        KINDS.iter().find(|(_, k)| *k == self).map(|(n, _)| *n).expect("every kind is named")
    }

    pub fn holds_children(self) -> bool {
        matches!(self, Kind::Row | Kind::Column | Kind::Drawer)
    }

    /// what this kind accepts beyond the style keys every node has.
    fn props(self) -> &'static [&'static str] {
        match self {
            Kind::Row | Kind::Column => &["children"],
            Kind::Drawer => &["children", "side", "width", "min", "max", "open"],
            Kind::Text => &["text"],
            Kind::Button => &["action", "icon", "label", "tooltip", "variant", "size"],
            Kind::Sessions => &["trace"],
            Kind::Transcript => &["user", "thoughts", "tools", "width", "gutter"],
            Kind::Input => &["bare"],
            Kind::Panel => &["tabs"],
            Kind::Panes => &["layout"],
            _ => &[],
        }
    }
}

const BASE_KEYS: [&str; 3] = ["type", "id", "when"];
pub const STYLE_KEYS: [&str; 33] = [
    "w", "h", "min_w", "max_w", "min_h", "max_h", "grow", "shrink", "p", "px", "py", "pt", "pr", "pb", "pl", "gap", "align", "justify", "bg", "color",
    "border", "border_color", "radius", "font", "weight", "opacity", "clip", "scroll", "drag", "inset", "shadow", "wrap", "hover",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub kind: Kind,
    pub id: Option<String>,
    pub when: Vec<Cond>,
    pub style: Style,
    pub props: Props,
    pub children: Vec<Node>,
    /// where it is in the file, like `root.children[1]`. names it in errors,
    /// and gives it an element id when it has none of its own.
    pub path: String,
}

impl Node {
    pub fn parse(v: &Value, at: &str) -> Result<Node, String> {
        let obj = v.as_object().ok_or_else(|| format!("{at}: expected a node, an object with a \"type\""))?;
        let ty = obj.get("type").ok_or_else(|| format!("{at}: a node needs a \"type\". types: {}", kind_names()))?;
        let name = text(ty, &format!("{at}.type"))?;
        let kind = KINDS.iter().find(|(n, _)| *n == name).map(|(_, k)| *k).ok_or_else(|| {
            let near = KINDS.iter().find(|(n, _)| close(n, name)).map(|(n, _)| format!(" (did you mean \"{n}\"?)")).unwrap_or_default();
            format!("{at}: there is no \"{name}\" component{near}. types: {}", kind_names())
        })?;
        let here = format!("{at} ({name})");
        let allowed: Vec<&str> = BASE_KEYS.iter().chain(STYLE_KEYS.iter()).chain(kind.props().iter()).copied().collect();
        keys(obj, &here, &allowed)?;

        let id = obj.get("id").map(|v| text(v, &format!("{here}.id")).map(str::to_owned)).transpose()?;
        if kind == Kind::Drawer && id.is_none() {
            return Err(format!("{here}: a drawer needs an \"id\", which buttons use to open and close it (\"toggle:<id>\")"));
        }
        let when = match obj.get("when") {
            None => Vec::new(),
            Some(Value::String(s)) => vec![Cond::parse(s, &format!("{here}.when"))?],
            Some(Value::Array(list)) => list.iter().map(|c| Cond::parse(text(c, &format!("{here}.when"))?, &format!("{here}.when"))).collect::<Result<_, _>>()?,
            Some(other) => return Err(format!("{here}.when: expected a condition or a list of them, got {other}")),
        };
        let style = Style::parse(obj, &here)?;
        let props = Props::parse(kind, obj, &here)?;
        let mut children = Vec::new();
        if let Some(c) = obj.get("children") {
            let list = c.as_array().ok_or_else(|| format!("{here}.children: expected a list of nodes"))?;
            for (i, child) in list.iter().enumerate() {
                children.push(Node::parse(child, &format!("{at}.children[{i}]"))?);
            }
        }
        Ok(Node { kind, id, when, style, props, children, path: at.to_owned() })
    }

    pub fn walk_mut(&mut self, f: &mut dyn FnMut(&mut Node)) {
        f(self);
        for c in &mut self.children {
            c.walk_mut(f);
        }
    }

    pub fn walk<'a>(&'a self, f: &mut dyn FnMut(&'a Node)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }

    /// the element id it is drawn with.
    pub fn element_id(&self) -> String {
        self.id.clone().unwrap_or_else(|| format!("ui:{}", self.path))
    }
}

fn kind_names() -> String {
    KINDS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
}

// ---- conditions --------------------------------------------------------------------------

/// when a node is shown. a node with several shows only when all of them hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Cond {
    /// a session is open (as opposed to the new-session form).
    Session,
    /// the open session's agent is working.
    Busy,
    /// the agent is waiting on you: a permission, a question or a plan.
    Pending,
    /// the session is in plan mode.
    Planning,
    /// files are waiting to go with the next message.
    Attachments,
    /// the daemon is reachable.
    Connected,
    /// the window is dark.
    Dark,
    /// a drawer is open.
    Open(String),
    /// the window is the frontmost one.
    Focused,
    Not(Box<Cond>),
}

impl Cond {
    pub const NAMES: [&str; 9] = ["session", "busy", "pending", "planning", "attachments", "connected", "dark", "focused", "open:<drawer>"];

    fn parse(s: &str, at: &str) -> Result<Cond, String> {
        if let Some(rest) = s.strip_prefix('!') {
            return Ok(Cond::Not(Box::new(Cond::parse(rest.trim(), at)?)));
        }
        Ok(match s {
            "session" => Cond::Session,
            "busy" => Cond::Busy,
            "pending" => Cond::Pending,
            "planning" => Cond::Planning,
            "attachments" => Cond::Attachments,
            "connected" => Cond::Connected,
            "dark" => Cond::Dark,
            "focused" => Cond::Focused,
            other => match other.strip_prefix("open:") {
                Some(id) if !id.is_empty() => Cond::Open(id.to_owned()),
                _ => return Err(format!("{at}: \"{s}\" isn't a condition. conditions: {} (put ! in front to negate)", Self::NAMES.join(", "))),
            },
        })
    }

    /// the condition with any negation taken off.
    pub fn inner(&self) -> &Cond {
        match self {
            Cond::Not(c) => c.inner(),
            c => c,
        }
    }
}

// ---- style -----------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Len {
    Px(f32),
    Full,
    Auto,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Paint {
    Token(Token, f32),
    Color(Hsla),
    Transparent,
}

impl Paint {
    pub fn resolve(self, p: &Palette) -> Hsla {
        match self {
            Paint::Token(t, alpha) => {
                let mut c = p.get(t);
                c.a *= alpha;
                c
            }
            Paint::Color(c) => c,
            Paint::Transparent => gpui_kit::transparent_black(),
        }
    }

    fn parse(v: &Value, at: &str) -> Result<Paint, String> {
        let s = text(v, at)?;
        if s == "transparent" {
            return Ok(Paint::Transparent);
        }
        if s.starts_with('#') {
            return hex(s).map(Paint::Color).ok_or_else(|| format!("{at}: \"{s}\" isn't a colour. write #rgb, #rrggbb or #rrggbbaa"));
        }
        // "accent/20" is the accent at 20% opacity.
        let (name, alpha) = match s.split_once('/') {
            Some((n, a)) => (n, a.parse::<f32>().map_err(|_| format!("{at}: \"{a}\" after the / should be a percentage, like accent/20"))? / 100.0),
            None => (s, 1.0),
        };
        let token = TOKENS.iter().find(|(n, _)| *n == name).map(|(_, t)| *t).ok_or_else(|| {
            let names: Vec<&str> = TOKENS.iter().map(|(n, _)| *n).collect();
            format!("{at}: \"{s}\" isn't a colour. use a role ({}), a role with an opacity (\"accent/20\"), \"transparent\" or #rrggbb", names.join(", "))
        })?;
        Ok(Paint::Token(token, alpha.clamp(0.0, 1.0)))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Sides {
    pub top: bool,
    pub right: bool,
    pub bottom: bool,
    pub left: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Font {
    Style(Type),
    Size(f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Stretch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Justify {
    Start,
    Center,
    End,
    Between,
}

/// room left at the leading edge for the window's close/minimise/zoom buttons.
#[derive(Clone, Debug, PartialEq)]
pub enum Inset {
    Always,
    /// only while this drawer is closed (it covers the buttons when open).
    Drawer(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Style {
    pub w: Option<Len>,
    pub h: Option<Len>,
    pub min_w: Option<f32>,
    pub max_w: Option<f32>,
    pub min_h: Option<f32>,
    pub max_h: Option<f32>,
    pub grow: bool,
    pub shrink: Option<bool>,
    pub p: Option<f32>,
    pub px: Option<f32>,
    pub py: Option<f32>,
    pub pt: Option<f32>,
    pub pr: Option<f32>,
    pub pb: Option<f32>,
    pub pl: Option<f32>,
    pub gap: Option<f32>,
    pub align: Option<Align>,
    pub justify: Option<Justify>,
    pub bg: Option<Paint>,
    pub color: Option<Paint>,
    pub border: Option<Sides>,
    pub border_color: Option<Paint>,
    pub radius: Option<f32>,
    pub font: Option<Font>,
    pub weight: Option<FontWeight>,
    pub opacity: Option<f32>,
    pub clip: bool,
    pub scroll: bool,
    pub drag: bool,
    pub inset: Option<Inset>,
    pub shadow: bool,
    pub wrap: bool,
    pub hover: Option<Paint>,
}

/// orchard's named radii.
pub const RADII: [(&str, f32); 7] = [("none", 0.0), ("tile", 8.0), ("control", 10.0), ("card", 14.0), ("panel", 18.0), ("widget", 22.0), ("full", 9999.0)];
/// orchard's three weights.
pub const WEIGHTS: [(&str, f32); 4] = [("regular", 450.0), ("medium", 550.0), ("semibold", 650.0), ("bold", 700.0)];
pub const FONTS: [(&str, Type); 7] = [
    ("title", Type::Title),
    ("headline", Type::Headline),
    ("body", Type::Body),
    ("callout", Type::Callout),
    ("subheadline", Type::Subheadline),
    ("prose", Type::Prose),
    ("code", Type::Code),
];

impl Style {
    fn parse(obj: &Map<String, Value>, at: &str) -> Result<Style, String> {
        let mut s = Style::default();
        let get = |k: &str| obj.get(k).map(|v| (v, format!("{at}.{k}")));
        let len = |v: &Value, at: &str| -> Result<Len, String> {
            match v {
                Value::String(t) if t == "full" => Ok(Len::Full),
                Value::String(t) if t == "auto" => Ok(Len::Auto),
                _ => num(v, at).map(Len::Px).map_err(|_| format!("{at}: expected points, \"full\" or \"auto\", got {v}")),
            }
        };
        if let Some((v, at)) = get("w") {
            s.w = Some(len(v, &at)?);
        }
        if let Some((v, at)) = get("h") {
            s.h = Some(len(v, &at)?);
        }
        for (key, slot) in [
            ("min_w", &mut s.min_w),
            ("max_w", &mut s.max_w),
            ("min_h", &mut s.min_h),
            ("max_h", &mut s.max_h),
            ("p", &mut s.p),
            ("px", &mut s.px),
            ("py", &mut s.py),
            ("pt", &mut s.pt),
            ("pr", &mut s.pr),
            ("pb", &mut s.pb),
            ("pl", &mut s.pl),
            ("gap", &mut s.gap),
            ("opacity", &mut s.opacity),
        ] {
            if let Some((v, at)) = get(key) {
                *slot = Some(num(v, &at)?);
            }
        }
        for (key, slot) in [("grow", &mut s.grow), ("clip", &mut s.clip), ("scroll", &mut s.scroll), ("drag", &mut s.drag), ("shadow", &mut s.shadow), ("wrap", &mut s.wrap)] {
            if let Some((v, at)) = get(key) {
                *slot = flag(v, &at)?;
            }
        }
        if let Some((v, at)) = get("shrink") {
            s.shrink = Some(flag(v, &at)?);
        }
        if let Some((v, at)) = get("align") {
            s.align = Some(one_of(v, &at, &[("start", Align::Start), ("center", Align::Center), ("end", Align::End), ("stretch", Align::Stretch)])?);
        }
        if let Some((v, at)) = get("justify") {
            s.justify = Some(one_of(v, &at, &[("start", Justify::Start), ("center", Justify::Center), ("end", Justify::End), ("between", Justify::Between)])?);
        }
        for (key, slot) in [("bg", &mut s.bg), ("color", &mut s.color), ("border_color", &mut s.border_color), ("hover", &mut s.hover)] {
            if let Some((v, at)) = get(key) {
                *slot = Some(Paint::parse(v, &at)?);
            }
        }
        if let Some((v, at)) = get("border") {
            s.border = Some(match v {
                Value::Bool(true) => Sides { top: true, right: true, bottom: true, left: true },
                Value::Bool(false) => Sides::default(),
                Value::String(_) | Value::Array(_) => {
                    let names: Vec<&str> = match v {
                        Value::String(one) => vec![one.as_str()],
                        Value::Array(list) => list.iter().map(|x| text(x, &at)).collect::<Result<_, _>>()?,
                        _ => unreachable!(),
                    };
                    let mut sides = Sides::default();
                    for n in names {
                        match n {
                            "top" => sides.top = true,
                            "right" => sides.right = true,
                            "bottom" => sides.bottom = true,
                            "left" => sides.left = true,
                            "all" => sides = Sides { top: true, right: true, bottom: true, left: true },
                            other => return Err(format!("{at}: \"{other}\" isn't a side. sides: top, right, bottom, left, all")),
                        }
                    }
                    sides
                }
                other => return Err(format!("{at}: expected true, a side (\"top\") or a list of sides, got {other}")),
            });
        }
        if let Some((v, at)) = get("radius") {
            s.radius = Some(match v {
                Value::String(_) => one_of(v, &at, &RADII)?,
                _ => num(v, &at)?,
            });
        }
        if let Some((v, at)) = get("font") {
            s.font = Some(match v {
                Value::String(_) => Font::Style(one_of(v, &at, &FONTS)?),
                _ => Font::Size(num(v, &at)?),
            });
        }
        if let Some((v, at)) = get("weight") {
            s.weight = Some(FontWeight(match v {
                Value::String(_) => one_of(v, &at, &WEIGHTS)?,
                _ => num(v, &at)?,
            }));
        }
        if let Some((v, at)) = get("inset") {
            s.inset = match v {
                Value::Bool(true) => Some(Inset::Always),
                Value::Bool(false) => None,
                Value::String(id) => Some(Inset::Drawer(id.clone())),
                other => return Err(format!("{at}: expected true or a drawer id, got {other}")),
            };
        }
        Ok(s)
    }
}

// ---- what a component can be told -------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// open or close a drawer.
    Toggle(String),
    NewSession,
    Overview,
    /// stop the agent.
    Stop,
    Send,
    Attach,
    /// go to the session that has waited longest.
    Next,
    /// allow the oldest permission request.
    Allow,
    /// hide everything but the session, or bring it back.
    Focus,
    /// start a session that changes this layout.
    Customize,
    /// the settings page.
    Settings,
}

impl Action {
    pub const NAMES: [&str; 11] = ["toggle:<drawer>", "new-session", "overview", "stop", "send", "attach", "next", "allow", "focus", "customize", "settings"];

    fn parse(s: &str, at: &str) -> Result<Action, String> {
        Ok(match s {
            "new-session" => Action::NewSession,
            "overview" => Action::Overview,
            "stop" => Action::Stop,
            "send" => Action::Send,
            "attach" => Action::Attach,
            "next" => Action::Next,
            "allow" => Action::Allow,
            "focus" => Action::Focus,
            "customize" => Action::Customize,
            "settings" => Action::Settings,
            other => match other.strip_prefix("toggle:") {
                Some(id) if !id.is_empty() => Action::Toggle(id.to_owned()),
                _ => return Err(format!("{at}: \"{s}\" isn't an action. actions: {}", Self::NAMES.join(", "))),
            },
        })
    }

    /// what a button with this action says when it says nothing else.
    pub fn tooltip(&self) -> String {
        match self {
            Action::Toggle(id) => format!("Show or Hide {}", sentence(id)),
            Action::NewSession => "New Session".into(),
            Action::Overview => "All Sessions".into(),
            Action::Stop => "Stop the Agent".into(),
            Action::Send => "Send".into(),
            Action::Attach => "Attach Images or PDFs".into(),
            Action::Next => "Next Session That Needs You".into(),
            Action::Allow => "Allow".into(),
            Action::Focus => "Focus on This Session".into(),
            Action::Customize => "Customize This Window…".into(),
            Action::Settings => "Settings".into(),
        }
    }
}

fn sentence(s: &str) -> String {
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Side {
    #[default]
    Left,
    Right,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Variant {
    #[default]
    Ghost,
    Primary,
    Outline,
    Text,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Size {
    XSmall,
    Small,
    #[default]
    Medium,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Show {
    #[default]
    Shown,
    /// only the ones that failed.
    Failed,
    Hidden,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Props {
    // drawer
    pub side: Side,
    pub width: Option<f32>,
    pub min: Option<f32>,
    pub max: Option<f32>,
    pub open: Option<bool>,
    // text
    pub text: Option<String>,
    // button
    pub action: Option<Action>,
    pub icon: Option<String>,
    pub label: Option<String>,
    pub tooltip: Option<String>,
    pub variant: Variant,
    pub size: Size,
    // sessions
    pub trace: Option<bool>,
    // transcript
    /// space either side of each row, inside its reading width.
    pub gutter: Option<f32>,
    /// your messages on the left; by default they're on the right, as a chat's are.
    pub user_left: bool,
    pub thoughts: Show,
    pub tools: Show,
    // input
    pub bare: bool,
    // panel
    pub tabs: Option<Vec<String>>,
    // panes
    pub layout: Option<Box<PaneTree>>,
}

pub const PANEL_TABS: [&str; 5] = ["changes", "files", "checks", "artifacts", "info"];

impl Props {
    fn parse(kind: Kind, obj: &Map<String, Value>, at: &str) -> Result<Props, String> {
        let mut p = Props::default();
        let get = |k: &str| obj.get(k).map(|v| (v, format!("{at}.{k}")));
        if let Some((v, at)) = get("side") {
            p.side = one_of(v, &at, &[("left", Side::Left), ("right", Side::Right)])?;
        }
        for (key, slot) in [("width", &mut p.width), ("min", &mut p.min), ("max", &mut p.max), ("gutter", &mut p.gutter)] {
            if let Some((v, at)) = get(key) {
                *slot = Some(num(v, &at)?);
            }
        }
        if let Some((v, at)) = get("open") {
            p.open = Some(flag(v, &at)?);
        }
        for (key, slot) in [("text", &mut p.text), ("icon", &mut p.icon), ("label", &mut p.label), ("tooltip", &mut p.tooltip)] {
            if let Some((v, at)) = get(key) {
                *slot = Some(text(v, &at)?.to_owned());
            }
        }
        if let Some((v, at)) = get("action") {
            p.action = Some(Action::parse(text(v, &at)?, &at)?);
        }
        if kind == Kind::Button && p.action.is_none() {
            return Err(format!("{at}: a button needs an \"action\". actions: {}", Action::NAMES.join(", ")));
        }
        if kind == Kind::Button && p.icon.is_none() && p.label.is_none() {
            return Err(format!("{at}: a button needs an \"icon\", a \"label\", or both"));
        }
        if let Some(icon) = &p.icon
            && !icon.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(format!("{at}.icon: \"{icon}\" isn't an icon name. icons are lucide names in kebab case, like \"panel-left\" or \"arrow-up\""));
        }
        if let Some((v, at)) = get("variant") {
            p.variant = one_of(v, &at, &[("ghost", Variant::Ghost), ("primary", Variant::Primary), ("outline", Variant::Outline), ("text", Variant::Text)])?;
        }
        if let Some((v, at)) = get("size") {
            p.size = one_of(v, &at, &[("xsmall", Size::XSmall), ("small", Size::Small), ("medium", Size::Medium)])?;
        }
        if let Some((v, at)) = get("trace") {
            p.trace = Some(flag(v, &at)?);
        }
        if let Some((v, at)) = get("user") {
            p.user_left = one_of(v, &at, &[("left", true), ("right", false)])?;
        }
        let shows = [("shown", Show::Shown), ("failed", Show::Failed), ("hidden", Show::Hidden)];
        if let Some((v, at)) = get("thoughts") {
            p.thoughts = one_of(v, &at, &[("shown", Show::Shown), ("hidden", Show::Hidden)])?;
        }
        if let Some((v, at)) = get("tools") {
            p.tools = one_of(v, &at, &shows)?;
        }
        if let Some((v, at)) = get("bare") {
            p.bare = flag(v, &at)?;
        }
        if kind == Kind::Panes {
            let (v, at) = get("layout").ok_or_else(|| format!("{at}: panes need a \"layout\": a pane like {{\"module\": \"conversation\"}}, or a split like {{\"split\": \"row\", \"children\": [...]}}"))?;
            let mut tree = PaneTree::parse(v, &at)?;
            tree.name_panes();
            p.layout = Some(Box::new(tree));
        }
        if let Some((v, at)) = get("tabs") {
            let list = v.as_array().ok_or_else(|| format!("{at}: expected a list of tabs from: {}", PANEL_TABS.join(", ")))?;
            let mut tabs = Vec::new();
            for t in list {
                let t = text(t, &at)?;
                if !PANEL_TABS.contains(&t) {
                    return Err(format!("{at}: \"{t}\" isn't a tab. tabs: {}", PANEL_TABS.join(", ")));
                }
                tabs.push(t.to_owned());
            }
            if tabs.is_empty() {
                return Err(format!("{at}: a panel needs at least one tab"));
            }
            p.tabs = Some(tabs);
        }
        Ok(p)
    }
}

// ---- panes ------------------------------------------------------------------------------

/// what a pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Module {
    /// the open session: its transcript, what is waiting on you, and the message box.
    Conversation,
    Sessions,
    /// changes, files, checks, artifacts and info as tabs in one pane.
    Workspace,
    Changes,
    Files,
    Checks,
    Artifacts,
    Info,
    /// live ui the agent drew for a pane (`tessel_surface` with region "pane").
    Surfaces,
    /// a shell the daemon runs, on ghostty's emulator.
    Terminal,
    /// history as a graph; staging, committing, undo.
    Git,
}

/// name, module, title, lucide icon, what it is.
pub const MODULES: [(&str, Module, &str, &str, &str); 11] = [
    ("conversation", Module::Conversation, "Conversation", "message-square", "The session: what the agent says and does, and the message box"),
    ("sessions", Module::Sessions, "Sessions", "list", "Every session, the ones that need you first"),
    ("workspace", Module::Workspace, "Workspace", "panels-top-left", "Changes, files, checks, artifacts and info as tabs"),
    ("changes", Module::Changes, "Changes", "git-compare", "What changed in the session's folder, with diffs"),
    ("files", Module::Files, "Files", "folder", "The session's folder, with an editor"),
    ("checks", Module::Checks, "Checks", "circle-check", "The branch, its pull request and CI"),
    ("artifacts", Module::Artifacts, "Artifacts", "sparkles", "Plans and files the agent made"),
    ("info", Module::Info, "Info", "info", "The session: model, cost, tools, folders"),
    ("surfaces", Module::Surfaces, "Surfaces", "layout-dashboard", "Live views the agent drew: progress, results, choices"),
    ("terminal", Module::Terminal, "Terminal", "square-terminal", "A shell in the session's folder, on Ghostty's core"),
    ("git", Module::Git, "Git", "git-graph", "History as a graph; stage, commit and undo"),
];

impl Module {
    pub fn name(self) -> &'static str {
        MODULES.iter().find(|m| m.1 == self).map(|m| m.0).expect("every module is named")
    }
    pub fn title(self) -> &'static str {
        MODULES.iter().find(|m| m.1 == self).map(|m| m.2).expect("every module is named")
    }
    pub fn icon(self) -> &'static str {
        MODULES.iter().find(|m| m.1 == self).map(|m| m.3).expect("every module is named")
    }
    pub fn about(self) -> &'static str {
        MODULES.iter().find(|m| m.1 == self).map(|m| m.4).expect("every module is named")
    }
    fn parse(v: &Value, at: &str) -> Result<Module, String> {
        let s = text(v, at)?;
        MODULES.iter().find(|m| m.0 == s).map(|m| m.1).ok_or_else(|| {
            let names: Vec<&str> = MODULES.iter().map(|m| m.0).collect();
            let near = names.iter().find(|n| close(n, s)).map(|n| format!(" (did you mean \"{n}\"?)")).unwrap_or_default();
            format!("{at}: there is no \"{s}\" module{near}. modules: {}", names.join(", "))
        })
    }
}

/// one tile of a `panes` node.
#[derive(Clone, Debug, PartialEq)]
pub struct Pane {
    /// how the pane is known: to buttons (`toggle:<id>`), to the window across
    /// reloads, and to the motion that carries it from where it was.
    pub id: String,
    pub given_id: bool,
    /// nothing is an empty pane, offering modules to put in it.
    pub module: Option<Module>,
    /// its share of the split it is in.
    pub size: f32,
    /// a closed pane takes no room, and opens again with `toggle:<id>`.
    pub open: bool,
    /// options for what it shows: the conversation's `user`, `thoughts`,
    /// `tools`, `width`, `gutter`; the session list's `trace`.
    pub options: Props,
    /// where it is, like `layout.children[1]`.
    pub path: String,
    /// the same, as child indices from the top of the layout.
    pub trail: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis {
    Row,
    Column,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PaneTree {
    Pane(Pane),
    Split { axis: Axis, size: f32, children: Vec<PaneTree>, path: String, trail: Vec<usize> },
}

impl PaneTree {
    fn parse(v: &Value, at: &str) -> Result<PaneTree, String> {
        Self::parse_at(v, at, Vec::new())
    }

    fn parse_at(v: &Value, at: &str, trail: Vec<usize>) -> Result<PaneTree, String> {
        let obj = v.as_object().ok_or_else(|| format!("{at}: expected a pane ({{\"module\": ...}}) or a split ({{\"split\": \"row\", \"children\": [...]}})"))?;
        let size = obj.get("size").map(|v| num(v, &format!("{at}.size"))).transpose()?.unwrap_or(1.0);
        if size <= 0.0 {
            return Err(format!("{at}.size: a share of the room has to be more than 0"));
        }
        if let Some(axis) = obj.get("split") {
            keys(obj, at, &["split", "size", "children"])?;
            let axis = one_of(axis, &format!("{at}.split"), &[("row", Axis::Row), ("column", Axis::Column)])?;
            let list = obj.get("children").and_then(Value::as_array).ok_or_else(|| format!("{at}: a split needs \"children\": the panes or splits in it"))?;
            if list.is_empty() {
                return Err(format!("{at}: a split needs at least one child"));
            }
            let children = list
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let mut t = trail.clone();
                    t.push(i);
                    PaneTree::parse_at(c, &format!("{at}.children[{i}]"), t)
                })
                .collect::<Result<_, _>>()?;
            return Ok(PaneTree::Split { axis, size, children, path: at.to_owned(), trail });
        }
        let options_keys = ["user", "thoughts", "tools", "width", "gutter", "trace"];
        let mut allowed = vec!["id", "module", "size", "open"];
        allowed.extend(options_keys);
        keys(obj, at, &allowed)?;
        let module = obj.get("module").map(|m| Module::parse(m, &format!("{at}.module"))).transpose()?;
        let mut opts = Map::new();
        for k in options_keys {
            if let Some(v) = obj.get(k) {
                opts.insert(k.to_owned(), v.clone());
            }
        }
        let options = Props::parse(Kind::Transcript, &opts, at)?;
        let id = obj.get("id").map(|v| text(v, &format!("{at}.id")).map(str::to_owned)).transpose()?;
        Ok(PaneTree::Pane(Pane {
            given_id: id.is_some(),
            id: id.unwrap_or_default(),
            module,
            size,
            open: obj.get("open").map(|v| flag(v, &format!("{at}.open"))).transpose()?.unwrap_or(true),
            options,
            path: at.to_owned(),
            trail,
        }))
    }

    /// panes without an id are known by their module, numbered if it repeats,
    /// or as `pane-1`, `pane-2` when empty.
    fn name_panes(&mut self) {
        let mut taken: Vec<String> = Vec::new();
        self.each_mut(&mut |p| {
            if p.given_id {
                taken.push(p.id.clone());
            }
        });
        self.each_mut(&mut |p| {
            if p.given_id {
                return;
            }
            let base = p.module.map_or("pane", Module::name);
            let mut n = 1;
            let mut id = base.to_owned();
            while taken.contains(&id) || (p.module.is_none() && n == 1) {
                n += 1;
                id = format!("{base}-{n}");
                if p.module.is_none() && n == 2 && !taken.contains(&format!("{base}-1")) {
                    id = format!("{base}-1");
                }
            }
            taken.push(id.clone());
            p.id = id;
        });
    }

    pub fn each_mut(&mut self, f: &mut dyn FnMut(&mut Pane)) {
        match self {
            PaneTree::Pane(p) => f(p),
            PaneTree::Split { children, .. } => children.iter_mut().for_each(|c| c.each_mut(f)),
        }
    }

    pub fn leaves<'a>(&'a self, out: &mut Vec<&'a Pane>) {
        match self {
            PaneTree::Pane(p) => out.push(p),
            PaneTree::Split { children, .. } => children.iter().for_each(|c| c.leaves(out)),
        }
    }

    pub fn size(&self) -> f32 {
        match self {
            PaneTree::Pane(p) => p.size,
            PaneTree::Split { size, .. } => *size,
        }
    }
}

/// a one-line summary of a layout, for a person reading a log.
pub fn describe(ui: &Ui) -> String {
    let mut out = String::new();
    let mut kinds: Vec<&str> = ui.nodes().iter().map(|n| n.kind.name()).collect();
    kinds.sort_unstable();
    kinds.dedup();
    let _ = write!(out, "{} nodes using {}", ui.nodes().len(), kinds.join(", "));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_is_valid_and_the_reference_mentions_every_component() {
        for (name, text) in PRESETS {
            let ui = parse(text).unwrap_or_else(|e| panic!("preset {name}: {e}"));
            assert!(ui.has_module(Module::Conversation) || ui.find(Kind::Transcript).is_some(), "{name} has a conversation");
        }
        for (name, ..) in MODULES {
            assert!(REFERENCE.contains(&format!("`{name}`")), "the reference documents the module {name}");
        }
        for (name, _) in KINDS {
            assert!(REFERENCE.contains(&format!("`{name}`")), "the reference documents {name}");
        }
        for key in STYLE_KEYS {
            assert!(REFERENCE.contains(&format!("`{key}`")), "the reference documents the style key {key}");
        }
        for (name, _) in TOKENS {
            assert!(REFERENCE.contains(&format!("`{name}`")), "the reference documents the colour {name}");
        }
    }

    #[test]
    fn a_preset_can_be_started_from_and_its_theme_changed() {
        let ui = parse(r##"{"theme": {"base": "orchard", "light": {"accent": "#123456"}}}"##).unwrap();
        assert_eq!(ui.root, Ui::default_layout().root, "a file that only sets a theme keeps the default layout");
        assert_eq!(ui.theme.base, Some(Base::Orchard));
        assert_eq!(ui.theme.palette(false).accent, hex("#123456").unwrap());
        assert_ne!(ui.theme.palette(true).accent, hex("#123456").unwrap(), "only the light accent was set");
    }

    #[test]
    fn mistakes_say_where_they_are_and_what_would_work() {
        let err = |json: &str| parse(json).unwrap_err();
        let e = err(r#"{"root": {"type": "colum"}}"#);
        assert!(e.contains("root") && e.contains("did you mean \"column\""), "{e}");
        let e = err(r#"{"root": {"type": "row", "children": [{"type": "text", "text": "hi", "colour": "accent"}]}}"#);
        assert!(e.contains("root.children[0]") && e.contains("\"colour\"") && e.contains("did you mean \"color\""), "{e}");
        let e = err(r#"{"root": {"type": "row", "bg": "acent"}}"#);
        assert!(e.contains("root (row).bg") && e.contains("accent"), "{e}");
        let e = err(r#"{"root": {"type": "button", "action": "toggle:nope", "icon": "x"}}"#);
        assert!(e.contains("toggle:nope") && e.contains("no drawers"), "{e}");
        let e = err(r#"{"root": {"type": "row", "when": "sesion"}}"#);
        assert!(e.contains("conditions:"), "{e}");
        let e = err(r#"{"preset": "fancy"}"#);
        assert!(e.contains("presets: default"), "{e}");
        let e = err(r#"{"root": {"type": "column", "children": [{"type": "input"}, {"type": "input"}]}}"#);
        assert!(e.contains("2 of conversation"), "{e}");
        let e = err("{not json");
        assert!(e.contains("not valid JSON"), "{e}");
    }

    #[test]
    fn colours_are_roles_with_an_opacity_or_hex() {
        let p = Palette::light();
        let paint = |s: &str| Paint::parse(&Value::String(s.into()), "x").unwrap().resolve(&p);
        assert_eq!(paint("accent"), p.accent);
        assert!((paint("accent/20").a - p.accent.a * 0.2).abs() < 1e-4);
        assert_eq!(paint("#ff0000"), hex("#f00").unwrap());
        assert_eq!(paint("transparent").a, 0.0);
        assert!(hex("#12345").is_none() && hex("123456").is_none() && hex("#ggg").is_none());
        assert!((hex("#00000080").unwrap().a - 128.0 / 255.0).abs() < 1e-4);
    }

    #[test]
    fn a_terminal_has_its_own_size_and_colours() {
        let t = parse(r#"{"theme": {"terminal": {"size": 15, "colors": "midnight"}}}"#).unwrap().theme;
        assert_eq!((t.term_size, t.term_colors.as_deref()), (Some(15.0), Some("midnight")));
        assert!(parse(r#"{"theme": {"terminal": {"size": 40}}}"#).unwrap_err().contains("terminal.size"));
        assert!(parse(r#"{"theme": {"terminal": {"colors": "neon"}}}"#).unwrap_err().contains("terminal.colors"));
    }

    #[test]
    fn the_backdrop_is_off_unless_asked_for() {
        assert_eq!(parse(r#"{"theme": "tessel"}"#).unwrap().theme.backdrop, None, "unsaid: the app's default, off");
        assert_eq!(parse(r#"{"theme": {"backdrop": "none"}}"#).unwrap().theme.backdrop, Some(Backdrop::None));
        assert_eq!(parse(r#"{"theme": {"backdrop": "dither"}}"#).unwrap().theme.backdrop, Some(Backdrop::Dither));
        assert_eq!(parse(r#"{"theme": {"backdrop": "space"}}"#).unwrap().theme.backdrop, Some(Backdrop::Space));
        assert!(parse(r#"{"theme": {"backdrop": "plaid"}}"#).unwrap_err().contains("theme.backdrop"));
    }

    #[test]
    fn the_common_options_can_be_set_without_writing_a_layout() {
        let ui = parse(r#"{"transcript": {"user": "left", "tools": "hidden"}, "sessions": {"trace": false}}"#).unwrap();
        let c = ui.panes().into_iter().find(|p| p.module == Some(Module::Conversation)).unwrap();
        assert!(c.options.user_left && c.options.tools == Show::Hidden && c.options.thoughts == Show::Shown, "the conversation pane took them");
        assert_eq!(ui.find(Kind::Sessions).unwrap().props.trace, Some(false));
        let e = parse(r#"{"transcript": {"side": "right"}}"#).unwrap_err();
        assert!(e.contains("transcript") && e.contains("user"), "{e}");
    }

    #[test]
    fn a_desktop_has_its_own_layout_and_shares_the_rest() {
        let file = r#"{"theme": "orchard", "root": {"type": "row", "children": [
                {"type": "drawer", "id": "sidebar", "children": [{"type": "sessions"}]},
                {"type": "panes", "layout": {"module": "conversation"}}]},
            "desktops": [{"name": "Main"}, {"id": "o", "name": "Orchard", "scope": ["orchard"], "layout": {"split": "row", "children": [{"module": "conversation"}, {"module": "terminal"}]}}],
            "desktop": "o"}"#;
        let ui = parse(file).unwrap();
        assert_eq!(ui.desktop, 1);
        assert_eq!(ui.desktops.iter().map(|d| (d.id.as_str(), d.name.as_str())).collect::<Vec<_>>(), [("d1", "Main"), ("o", "Orchard")]);
        assert_eq!(ui.desktops[1].scope, ["orchard"]);
        assert_eq!(ui.theme.base, Some(Base::Orchard), "the theme is the file's, on every desktop");
        assert_eq!(ui.panes().len(), 2, "Orchard's own panes");
        assert!(ui.find(Kind::Sessions).is_some(), "in the shared sidebar");
        let main = parse(&file.replace(r#""desktop": "o""#, r#""desktop": "d1""#)).unwrap();
        assert_eq!(main.panes().len(), 1, "Main has no panes of its own: the file's");
        // an entry written before desktops held only panes: its panes, the sidebar still shared.
        let old = parse(&file.replace(r#""layout": {"split": "row", "children": [{"module": "conversation"}, {"module": "terminal"}]}"#, r#""preset": "tiles""#)).unwrap();
        assert!(old.panes().iter().any(|p| p.module == Some(Module::Terminal)) && old.find(Kind::Sessions).is_some());
        assert_eq!(parse(r#"{"desktops": [{"name": "A"}, {"name": "B"}], "desktop": 2}"#).unwrap().desktop, 1, "a number counts from one");
        assert!(parse(r#"{"desktops": [{"id": "x"}, {"id": "x"}]}"#).unwrap_err().contains("used twice"));
        assert!(parse(r#"{"desktops": [{"name": "A", "scope": "orchard"}]}"#).unwrap_err().contains("scope"));
        assert!(parse(r#"{}"#).unwrap().desktops.is_empty(), "no list: one desktop, as before");
    }

    #[test]
    fn a_layout_made_before_desktops_gets_them_beside_settings() {
        let old = parse(r#"{"root": {"type": "row", "children": [{"type": "row", "children": [{"type": "button", "id": "settings", "action": "settings", "icon": "settings"}]}]}}"#).unwrap();
        let row = &old.root.children[0];
        assert_eq!(row.children.iter().map(|c| c.kind).collect::<Vec<_>>(), [Kind::Button, Kind::Desktops]);
        let placed = parse(r#"{"root": {"type": "row", "children": [{"type": "desktops"}, {"type": "button", "id": "settings", "action": "settings", "icon": "settings"}]}}"#).unwrap();
        assert_eq!(placed.nodes().iter().filter(|n| n.kind == Kind::Desktops).count(), 1, "one placed by hand stays where it is, alone");
        assert_eq!(Ui::default_layout().nodes().iter().filter(|n| n.kind == Kind::Desktops).count(), 1);
    }

    #[test]
    fn the_notch_follows_the_screen_unless_set() {
        assert_eq!(Ui::default_layout().notch, None, "decided by whether the screen has one");
        assert_eq!(parse(r#"{"theme": "orchard"}"#).unwrap().notch, None);
        assert_eq!(parse(r#"{"notch": false}"#).unwrap().notch, Some(false));
        assert_eq!(parse(r#"{"notch": true}"#).unwrap().notch, Some(true));
        assert!(parse(r#"{"notch": "off"}"#).unwrap_err().contains("notch"));
    }

    #[test]
    fn panes_are_named_split_and_checked() {
        let ui = parse(r#"{"root": {"type": "panes", "drag": true, "inset": true, "layout": {"split": "row", "children": [
            {"module": "conversation", "size": 2},
            {"split": "column", "children": [{"module": "changes"}, {}, {"id": "notes", "module": "files", "open": false}]}
        ]}}}"#).unwrap();
        let ids: Vec<&str> = ui.panes().iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["conversation", "changes", "pane-1", "notes"]);
        assert_eq!(ui.panes()[2].trail, [1, 1]);
        assert!(!ui.panes()[3].open);
        // two chats side by side are fine; a loose transcript beside a conversation pane is not.
        assert!(parse(r#"{"root": {"type": "panes", "layout": {"split": "row", "children": [{"module": "conversation"}, {"module": "conversation"}]}}}"#).is_ok());
        let e = parse(r#"{"root": {"type": "row", "children": [{"type": "transcript"}, {"type": "panes", "layout": {"module": "conversation"}}]}}"#).unwrap_err();
        assert!(e.contains("conversation") && e.contains("there can be one"), "{e}");
        let e = parse(r#"{"root": {"type": "panes", "layout": {"module": "chnages"}}}"#).unwrap_err();
        assert!(e.contains("did you mean \"changes\""), "{e}");
        let ok = parse(r#"{"root": {"type": "column", "drag": true, "inset": true, "children": [
            {"type": "button", "action": "toggle:notes", "icon": "x"},
            {"type": "panes", "layout": {"split": "row", "children": [{"module": "conversation"}, {"id": "notes", "module": "files"}]}}
        ]}}"#);
        assert!(ok.is_ok(), "toggle names a pane: {ok:?}");
    }

    #[test]
    fn conditions_negate_and_name_drawers() {
        assert_eq!(Cond::parse("!busy", "x").unwrap(), Cond::Not(Box::new(Cond::Busy)));
        assert_eq!(Cond::parse("open:sidebar", "x").unwrap().inner(), &Cond::Open("sidebar".into()));
        assert!(Cond::parse("open:", "x").is_err());
    }

    #[test]
    fn the_panel_and_its_drawer_are_found() {
        let ui = parse(r#"{"root": {"type": "row", "drag": true, "inset": true, "children": [
            {"type": "drawer", "id": "sidebar", "children": [{"type": "sessions"}]},
            {"type": "column", "children": [{"type": "transcript"}, {"type": "input"}]},
            {"type": "drawer", "id": "inspector", "side": "right", "children": [{"type": "panel"}]}
        ]}}"#).unwrap();
        assert_eq!(ui.drawer_holding(Kind::Panel).as_deref(), Some("inspector"));
        assert_eq!(ui.drawer_holding(Kind::Sessions).as_deref(), Some("sidebar"));
        assert_eq!(ui.drawer_holding(Kind::Transcript), None);
    }
}
