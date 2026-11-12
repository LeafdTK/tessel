//! desktops: several layouts, each with what was on it.
//!
//! a desktop is a view, not a container. it has a layout of its own, the
//! projects its sidebar lists, and whatever chats its panes held; sessions
//! belong to the daemon and run whichever desktop is showing. leaving a
//! desktop keeps its chats followed and its scroll where it was, so coming
//! back is instant and nothing is fetched twice.
//!
//! in `ui.json` they are a `desktops` list and a `desktop` key saying which is
//! showing (see `ui::desktop_overlay`). switching writes that key, and the file
//! reloading is what changes the window, so a desktop changed by hand, by a
//! script or by an agent all go the same way.

use super::chats::ChatView;
use super::Shell;
use crate::motion::{PANEL, Spring};
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::*;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use tessel_proto::SessionId;

/// what a desktop had on it when you left.
pub(crate) struct Stashed {
    pub(crate) open: Option<SessionId>,
    pub(crate) chat_pane: Option<String>,
    pub(crate) views: HashMap<String, ChatView>,
    pub(crate) scroller: Entity<MessageScrollerState>,
    pub(crate) pane_focus: Option<String>,
    pub(crate) show_new: bool,
}

/// the desktop a layout shows: its id. a file without desktops is "d1", the
/// id its layout gets when a second desktop is made, so nothing is lost then.
pub(crate) fn desktop_key(ui: &crate::ui::Ui) -> String {
    ui.desktops.get(ui.desktop).map_or_else(|| "d1".to_owned(), |d| d.id.clone())
}

impl Shell {
    /// everything the window keeps of the desktop it's leaving.
    pub(crate) fn leave_desktop(&mut self, key: String, cx: &mut Context<Self>) {
        let open = self.model.open.as_ref().map(|o| o.id).or(self.awaiting);
        // the draft stays with its chat, for wherever the chat shows next.
        let text = self.composer.read(cx).value().to_string();
        if let Some(o) = open {
            if text.trim().is_empty() {
                self.drafts.remove(&o);
            } else {
                self.drafts.insert(o, text);
            }
        }
        if let Some(o) = self.model.open.take() {
            self.model.others.insert(o.id, o);
        }
        self.awaiting = None;
        let scroller = std::mem::replace(&mut self.scroller, cx.new(|cx| MessageScrollerState::new(0, cx)));
        let stashed = Stashed {
            open,
            chat_pane: self.chat_pane.take(),
            views: std::mem::take(&mut self.views),
            scroller,
            pane_focus: self.pane_focus.take(),
            show_new: self.show_new,
        };
        self.stashed.insert(key, stashed);
        self.answers.clear();
        self.prompt_open.snap(0.0);
        self.prompt_ghost.clear();
    }

    /// put back what the desktop had, or, the first time, the chat it had
    /// open last, else the latest in its projects, else a new chat.
    pub(crate) fn enter_desktop(&mut self, key: &str, cx: &mut Context<Self>) {
        match self.stashed.remove(key) {
            Some(st) => {
                self.scroller = st.scroller;
                self.chat_pane = st.chat_pane;
                self.views = st.views;
                self.pane_focus = st.pane_focus;
                self.show_new = st.show_new;
                match st.open {
                    Some(s) => self.switch_open(s, true, cx),
                    None => self.draft_to_set = Some(String::new()),
                }
            }
            None => {
                let saved = self.ui.desktops.get(self.ui.desktop).and_then(|d| d.open.as_deref()).and_then(|s| s.parse::<uuid::Uuid>().ok()).map(SessionId);
                let scope = self.model.scope.clone();
                let latest = self
                    .model
                    .sessions
                    .iter()
                    .filter(|s| scope.is_empty() || scope.contains(&crate::model::project_name(s)))
                    .max_by_key(|s| s.updated_ms.max(s.created_ms))
                    .map(|s| s.id);
                match saved.filter(|s| self.model.sessions.iter().any(|x| x.id == *s)).or(latest) {
                    Some(s) => {
                        self.show_new = false;
                        self.switch_open(s, false, cx);
                    }
                    None => {
                        self.show_new = true;
                        self.draft_to_set = Some(String::new());
                    }
                }
            }
        }
        self.sync_follow();
    }

    /// the chats on desktops you aren't looking at, still followed.
    pub(crate) fn stashed_sessions(&self) -> Vec<SessionId> {
        let mut out = Vec::new();
        for st in self.stashed.values() {
            out.extend(st.open);
            out.extend(st.views.values().map(|v| v.session));
        }
        out
    }

    /// ⌘1…⌘9: that desktop, when there is one.
    pub(crate) fn go_desktop(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if n >= 1 && n <= self.ui.desktops.len() && n - 1 != self.ui.desktop {
            self.use_desktop(n - 1, window, cx);
        }
    }

    /// slide the new desktop in from the side it sits on.
    pub(crate) fn slide_desktop(&mut self, from: usize, to: usize) {
        let dir = if to > from { 1.0 } else { -1.0 };
        self.desktop_slide = Spring::unit(dir);
        self.desktop_slide.to(0.0, PANEL);
    }

    // ---- the list in ui.json ---------------------------------------------------------

    /// the file's desktops, made if it has none yet: the layout in use becomes
    /// "Main", so nothing on screen changes by gaining a second desktop.
    fn desktops_in(file: &mut Map<String, Value>) -> &mut Vec<Value> {
        if !matches!(file.get("desktops"), Some(Value::Array(a)) if !a.is_empty()) {
            file.insert("desktops".into(), json!([{"id": "d1", "name": "Main"}]));
            file.insert("desktop".into(), json!("d1"));
        }
        match file.get_mut("desktops") {
            Some(Value::Array(a)) => a,
            _ => unreachable!("just made"),
        }
    }

    /// a desktop named by its number (from 1), its name or its id; none: the one showing.
    pub(crate) fn desktop_index(&self, which: Option<&Value>) -> Result<usize, String> {
        let list = &self.ui.desktops;
        let names = || list.iter().map(|d| d.name.clone()).collect::<Vec<_>>().join(", ");
        match which {
            None | Some(Value::Null) => Ok(self.ui.desktop),
            Some(Value::Number(n)) => {
                let n = n.as_u64().unwrap_or(0) as usize;
                if n >= 1 && n <= list.len().max(1) {
                    Ok(n - 1)
                } else {
                    Err(format!("there's no desktop {n}. there are {}", list.len().max(1)))
                }
            }
            Some(Value::String(s)) => list
                .iter()
                .position(|d| d.id == *s || d.name.eq_ignore_ascii_case(s))
                .or_else(|| s.parse::<usize>().ok().filter(|n| *n >= 1 && *n <= list.len()).map(|n| n - 1))
                .ok_or_else(|| format!("there's no desktop \"{s}\". desktops: {}", names())),
            Some(other) => Err(format!("desktop: expected a number, a name or an id, got {other}")),
        }
    }

    /// show desktop `to`, remembering the chat the one you leave had open.
    pub(crate) fn use_desktop(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.model.open.as_ref().map(|o| o.id.0.to_string());
        let from = self.ui.desktop;
        self.edit_ui(
            |f| {
                let list = Self::desktops_in(f);
                if let Some(Value::Object(d)) = list.get_mut(from) {
                    match &open {
                        Some(id) => {
                            d.insert("open".into(), json!(id));
                        }
                        None => {
                            d.remove("open");
                        }
                    }
                }
                if let Some(id) = list.get(to).and_then(|d| d.get("id")).cloned().or_else(|| Some(json!(format!("d{}", to + 1)))) {
                    f.insert("desktop".into(), id);
                }
            },
            window,
            cx,
        );
    }

    /// a new desktop after the last, shown at once. `copy` starts it with the
    /// layout in use; otherwise `preset`, else the default layout.
    pub(crate) fn new_desktop(&mut self, name: Option<String>, preset: Option<String>, copy: bool, scope: Vec<String>, window: &mut Window, cx: &mut Context<Self>) -> Value {
        let current_root = self.ui_file().get("root").cloned();
        let mut made = Value::Null;
        let open = self.model.open.as_ref().map(|o| o.id.0.to_string());
        let from = self.ui.desktop;
        self.edit_ui(
            |f| {
                // what this desktop's layout is now, in case the new one copies it.
                let showing_root = Self::desktops_in(f).get(from).and_then(|d| d.get("root")).cloned().or(current_root);
                let list = Self::desktops_in(f);
                if let (Some(Value::Object(d)), Some(id)) = (list.get_mut(from), &open) {
                    d.insert("open".into(), json!(id));
                }
                let taken: Vec<String> = list.iter().filter_map(|d| d.get("id").and_then(Value::as_str).map(str::to_owned)).collect();
                let id = (1..).map(|n| format!("d{n}")).find(|id| !taken.contains(id)).expect("there's always a free one");
                let n = list.len() + 1;
                let mut d = Map::new();
                d.insert("id".into(), json!(id));
                d.insert("name".into(), json!(name.clone().unwrap_or_else(|| format!("Desktop {n}"))));
                if !scope.is_empty() {
                    d.insert("scope".into(), json!(scope));
                }
                match (copy, preset.clone(), showing_root) {
                    (true, _, Some(root)) => {
                        d.insert("root".into(), root);
                    }
                    (_, Some(p), _) => {
                        d.insert("preset".into(), json!(p));
                    }
                    _ => {
                        d.insert("preset".into(), json!("default"));
                    }
                }
                list.push(Value::Object(d));
                f.insert("desktop".into(), json!(id));
                made = json!({"desktop": id, "number": n});
            },
            window,
            cx,
        );
        made
    }

    /// change one desktop's entry in the file.
    pub(crate) fn edit_desktop(&mut self, at: usize, change: impl FnOnce(&mut Map<String, Value>), window: &mut Window, cx: &mut Context<Self>) {
        self.edit_ui(
            |f| {
                if let Some(Value::Object(d)) = Self::desktops_in(f).get_mut(at) {
                    change(d);
                }
            },
            window,
            cx,
        );
    }

    /// forget a desktop. its chats keep running; they're a click away on any other.
    pub(crate) fn close_desktop(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if self.ui.desktops.len() <= 1 {
            return Err("this is the only desktop".into());
        }
        let showing = self.ui.desktop;
        let gone = self.ui.desktops[at].id.clone();
        self.edit_ui(
            |f| {
                let list = Self::desktops_in(f);
                if at < list.len() {
                    list.remove(at);
                }
                // the one beside it shows, as a closed tab's neighbour does.
                if at == showing {
                    let next = at.min(list.len().saturating_sub(1));
                    if let Some(id) = list.get(next).and_then(|d| d.get("id")).cloned() {
                        f.insert("desktop".into(), id);
                    }
                }
            },
            window,
            cx,
        );
        self.stashed.remove(&gone);
        self.sync_follow();
        Ok(())
    }

    /// change the layout of the desktop showing: its own, made from the one it
    /// shares the first time it's changed.
    pub(crate) fn desktop_root_mut(file: &mut Map<String, Value>, showing: usize, current: Option<Value>) -> Option<&mut Value> {
        if !matches!(file.get("desktops"), Some(Value::Array(a)) if !a.is_empty()) {
            if !file.contains_key("root") {
                file.insert("root".into(), current?);
                file.remove("preset");
            }
            return file.get_mut("root");
        }
        let shared = file.get("root").cloned();
        let Some(Value::Object(d)) = Self::desktops_in(file).get_mut(showing) else { return None };
        if !d.contains_key("root") {
            let preset = d.remove("preset").and_then(|p| p.as_str().map(str::to_owned));
            let root = match preset {
                Some(name) => crate::ui::preset(&name).and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("root").cloned()),
                None => shared.or(current),
            };
            d.insert("root".into(), root?);
        }
        d.get_mut("root")
    }
}
