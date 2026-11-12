//! running a command from the registry (`crate::commands`), whoever asked:
//! a key, the palette, `tesselctl ui`, or an agent's tool. answers are json,
//! errors are a sentence that says what would have worked.

use super::*;
use crate::ui::{Axis, Module, MODULES};
use serde_json::{Value, json};

fn arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn need<'a>(args: &'a Value, key: &str, cmd: &str) -> Result<&'a str, String> {
    arg(args, key).ok_or_else(|| format!("{cmd} needs \"{key}\" ({})", crate::commands::spec(cmd).map_or("", |s| s.args)))
}

impl Shell {
    /// run a command. `args` is a json object (null is taken as `{}`).
    pub(crate) fn exec(&mut self, cmd: &str, args: &Value, window: &mut Window, cx: &mut Context<Self>) -> Result<Value, String> {
        let done = Ok(Value::Null);
        let out = match cmd {
            "app.palette" => {
                self.open_palette(cx);
                done
            }
            "app.settings" => {
                if let Some(section) = arg(args, "section") {
                    let found = super::settings::SECTIONS.iter().find(|(id, title, _)| *id == section || title.eq_ignore_ascii_case(section));
                    let Some((id, ..)) = found else {
                        let names: Vec<&str> = super::settings::SECTIONS.iter().map(|(id, ..)| *id).collect();
                        return Err(format!("there's no section \"{section}\". sections: {}", names.join(", ")));
                    };
                    self.settings_section = id;
                }
                self.open_settings(cx);
                done
            }
            "app.overview" => {
                self.toggle_overview(cx);
                done
            }
            "app.sidebar" => {
                self.toggle_sidebar(cx);
                done
            }
            "app.workspace" => {
                self.toggle_inspector(cx);
                done
            }
            "app.focus" => {
                self.toggle_focus_mode(cx);
                done
            }

            "session.new" => match arg(args, "prompt") {
                Some(prompt) => {
                    let cwd = arg(args, "cwd").map(str::to_owned).or_else(|| self.new_folder()).ok_or("no project to start in: give cwd")?;
                    let worktree = args.get("worktree").and_then(Value::as_bool).unwrap_or(false);
                    self.cmd(Cmd::Create { cwd, worktree, prompt: prompt.to_owned() });
                    done
                }
                None => {
                    if let Some(cwd) = arg(args, "cwd") {
                        self.start_in(cwd.to_owned(), window, cx);
                    }
                    self.show_new_session(cx);
                    done
                }
            },
            "session.next" => {
                self.next_attention(cx);
                done
            }
            "session.open" => {
                let id = self.find_session(need(args, "session", cmd)?)?;
                self.open(id, cx);
                Ok(json!({"session": id.to_string()}))
            }
            "composer.attach" => {
                self.choose_attachments(window, cx);
                done
            }
            "session.stop" => {
                let id = self.session_arg(args)?;
                self.cmd(Cmd::Interrupt(id));
                done
            }
            "session.allow" => {
                self.allow_pending(cx);
                done
            }
            "session.send" => {
                let id = self.session_arg(args)?;
                self.cmd(Cmd::Send { session: id, text: need(args, "text", cmd)?.to_owned(), attachments: Vec::new() });
                done
            }
            "session.rename" => {
                let id = self.session_arg(args)?;
                self.cmd(Cmd::SetGoal { session: id, goal: need(args, "name", cmd)?.to_owned() });
                done
            }
            "session.delete" => {
                let id = self.find_session(need(args, "session", cmd)?)?;
                // scripted deletes are deliberate: no second press.
                self.cmd(Cmd::Delete { session: id, remove_worktree: false });
                done
            }
            "session.model" => {
                let id = self.session_arg(args)?;
                self.cmd(Cmd::SetModel { session: id, model: need(args, "model", cmd)?.to_owned() });
                done
            }
            "model.favorite" => {
                let id = self.session_arg(args)?;
                let favs = self.favorites();
                let n = args.get("n").and_then(Value::as_u64).ok_or("say which favourite: n, from 1")? as usize;
                let fav = favs.get(n.wrapping_sub(1)).cloned().ok_or_else(|| format!("there are {} favourites", favs.len()))?;
                self.pick_favorite(id, &fav, cx);
                Ok(serde_json::to_value(&fav).unwrap_or_default())
            }
            "model.effort" => {
                let id = self.session_arg(args)?;
                let effort = match arg(args, "effort") {
                    Some("default") => None,
                    Some(e) if tessel_proto::EFFORTS.contains(&e) => Some(e.to_owned()),
                    Some(e) => return Err(format!("\"{e}\" isn't an effort. efforts: {}, default", tessel_proto::EFFORTS.join(", "))),
                    None => self.next_effort(id),
                };
                self.set_effort(id, effort.clone(), cx);
                Ok(serde_json::json!({ "effort": effort }))
            }
            "model.fast" => {
                let id = self.session_arg(args)?;
                let want = args.get("fast").and_then(Value::as_bool).unwrap_or(!self.session_fast(id));
                let on = self.set_fast(id, want, cx);
                if want && !on {
                    return Err("this model has no fast mode".into());
                }
                Ok(serde_json::json!({ "fast": on }))
            }
            "model.list" => Ok(serde_json::json!({
                "models": self.models,
                "favorites": self.favorites(),
            })),
            "model.favorites" => {
                let favs: Vec<tessel_proto::FavoriteModel> =
                    serde_json::from_value(args.get("models").cloned().ok_or("give the favourites: models")?).map_err(|e| e.to_string())?;
                if favs.len() > tessel_proto::MAX_FAVORITES {
                    return Err(format!("at most {} favourites", tessel_proto::MAX_FAVORITES));
                }
                self.save_favorites(favs, cx);
                done
            }
            "session.mode" => {
                let id = self.session_arg(args)?;
                let mode = need(args, "mode", cmd)?;
                let modes: Vec<&str> = super::composer::MODES.iter().map(|m| m.0).collect();
                if !modes.contains(&mode) {
                    return Err(format!("\"{mode}\" isn't a mode. modes: {}", modes.join(", ")));
                }
                self.cmd(Cmd::SetMode { session: id, mode: mode.to_owned() });
                done
            }

            "theme.use" => {
                let name = arg(args, "name").ok_or("say which theme: name (theme.list lists them)")?.to_owned();
                self.use_theme(&name, window, cx)?;
                done
            }
            "theme.list" => {
                let current = self.theme_list().into_iter().map(|t| json!({"name": t.key, "title": t.name, "about": t.about, "current": crate::ui::themes::is_current(&t, &self.ui.theme)})).collect::<Vec<_>>();
                Ok(json!(current))
            }
            "theme.density" => {
                let d = arg(args, "density").ok_or("density: islands or compact")?;
                if !matches!(d, "islands" | "compact") {
                    return Err(format!("\"{d}\" isn't a density. use islands or compact"));
                }
                let v = (d != "islands").then(|| json!(d));
                self.edit_ui(|f| Self::set_in(f, "theme", "density", v), window, cx);
                done
            }
            "theme.material" => {
                let m = arg(args, "material").ok_or("material: solid, glass or felt")?;
                if !matches!(m, "solid" | "glass" | "felt") {
                    return Err(format!("\"{m}\" isn't a material. use solid, glass or felt"));
                }
                let v = (m != "solid").then(|| json!(m));
                self.edit_ui(|f| Self::set_in(f, "theme", "material", v), window, cx);
                done
            }
            "theme.backdrop" => {
                let b = arg(args, "backdrop").ok_or("backdrop: none, dither or space")?;
                if !matches!(b, "none" | "dither" | "space") {
                    return Err(format!("\"{b}\" isn't a backdrop. use none, dither or space"));
                }
                let v = (b != "none").then(|| json!(b));
                self.edit_ui(|f| Self::set_in(f, "theme", "backdrop", v), window, cx);
                done
            }
            "backdrop.next" => {
                self.next_space_photo(cx);
                done
            }
            "layout.use" => {
                let name = arg(args, "name").ok_or("say which layout: studio, focus, tiles or rail")?.to_owned();
                self.use_layout(&name, window, cx)?;
                done
            }
            "files.open" => {
                let path = arg(args, "path").ok_or("say which file: path, relative to the session's folder")?.trim_start_matches("./").to_owned();
                if self.ws_session.is_none() {
                    return Err("open a session first: files are in its folder".into());
                }
                self.open_file(path.clone(), cx);
                Ok(json!({"opening": path}))
            }
            "terminal.new" => {
                let pane = self.new_terminal(window, cx).ok_or("there's no pane to put a terminal beside")?;
                Ok(json!({"pane": pane}))
            }
            "pane.split" => {
                let pane = self.pane_arg(args)?;
                let axis = match arg(args, "dir").unwrap_or("right") {
                    "right" => Axis::Row,
                    "down" => Axis::Column,
                    other => return Err(format!("\"{other}\" isn't a direction. use right or down")),
                };
                // the answer names the new pane (focused now), so a script can go on with it.
                let new = self.split_pane(pane.trail, axis, window, cx);
                Ok(json!({"pane": new}))
            }
            "pane.close" => {
                let pane = self.pane_arg(args)?;
                if self.pane_focus.as_deref() == Some(pane.id.as_str()) {
                    self.pane_focus = None;
                }
                self.close_pane(pane.trail, window, cx);
                Ok(json!({"closed": pane.id}))
            }
            "pane.zoom" => {
                let pane = self.pane_arg(args)?;
                self.zoom_pane(&pane.id, cx);
                Ok(json!({"zoomed": self.pane_zoom}))
            }
            "pane.show" => {
                let pane = self.pane_arg(args)?;
                let name = need(args, "module", cmd)?;
                let module = MODULES.iter().find(|m| m.0 == name).map(|m| m.1).ok_or_else(|| {
                    format!("there's no \"{name}\" module. modules: {}", MODULES.iter().map(|m| m.0).collect::<Vec<_>>().join(", "))
                })?;
                if self.shown_elsewhere(module, Some(pane.id.as_str())) {
                    return Err(format!("{name} is already in the window, and there can be one"));
                }
                self.set_pane_module(pane.trail, module, window, cx);
                done
            }
            "pane.focus" => {
                let pane = self.find_pane(need(args, "pane", cmd)?)?;
                self.pane_focus = Some(pane.id.clone());
                self.publish("pane_focused", json!({"pane": pane.id}));
                cx.notify();
                done
            }
            "pane.toggle" => {
                let pane = self.find_pane(need(args, "pane", cmd)?)?;
                self.toggle_pane(&pane.id, cx);
                done
            }
            "pane.move" => {
                let from = self.find_pane(need(args, "pane", cmd)?)?;
                let to = self.find_pane(need(args, "to", cmd)?)?;
                let zone = match arg(args, "zone").unwrap_or("swap") {
                    "left" => panes::Zone::Left,
                    "right" => panes::Zone::Right,
                    "top" => panes::Zone::Top,
                    "bottom" => panes::Zone::Bottom,
                    "swap" => panes::Zone::Swap,
                    other => return Err(format!("\"{other}\" isn't a zone. zones: left, right, top, bottom, swap")),
                };
                self.move_pane_to(from.id, to.id, zone, window, cx);
                done
            }
            "workspace.tab" => {
                let name = need(args, "tab", cmd)?;
                let tab = Tab::ALL.into_iter().find(|t| t.title().eq_ignore_ascii_case(name)).ok_or_else(|| format!("\"{name}\" isn't a tab. tabs: changes, files, checks, artifacts, info"))?;
                self.set_tab(tab, cx);
                done
            }

            "theme.set" => {
                let base = need(args, "base", cmd)?;
                if !["tessel", "orchard", "tern"].contains(&base) {
                    return Err(format!("\"{base}\" isn't a theme. themes: tessel, orchard, tern"));
                }
                let b = base.to_owned();
                self.edit_ui(|f| Self::set_in(f, "theme", "base", Some(json!(b))), window, cx);
                done
            }
            "theme.appearance" => {
                let mode = need(args, "mode", cmd)?;
                if !["system", "light", "dark"].contains(&mode) {
                    return Err(format!("\"{mode}\" isn't an appearance. use system, light or dark"));
                }
                let m = mode.to_owned();
                self.edit_ui(|f| Self::set_in(f, "theme", "appearance", (m != "system").then(|| json!(m))), window, cx);
                done
            }
            "theme.size" => {
                let size = need(args, "size", cmd)?;
                if !crate::theme::SCALES.iter().any(|(n, _)| *n == size) {
                    return Err(format!("\"{size}\" isn't a size. use compact, default or large"));
                }
                let s = size.to_owned();
                self.edit_ui(|f| Self::set_in(f, "theme", "size", (s != "default").then(|| json!(s))), window, cx);
                done
            }
            "theme.accent" => {
                let hex = arg(args, "hex").map(str::to_owned);
                if let Some(h) = &hex
                    && crate::ui::hex(h).is_none()
                {
                    return Err(format!("\"{h}\" isn't a colour. write #rrggbb"));
                }
                self.set_accent(hex, window, cx);
                done
            }

            "daemon.set" => {
                let patch = args.as_object().filter(|o| !o.is_empty()).ok_or("say what to set, like {\"agent_tools\": false}")?;
                if self.daemon_settings.is_none() {
                    return Err("the daemon running now is too old to change settings. restart it, then try again".into());
                }
                // the daemon checks it; what it ends up as comes back to everyone.
                self.cmd(Cmd::SetSettings(Value::Object(patch.clone())));
                done
            }

            "layout.reset" => {
                self.edit_ui(
                    |f| {
                        f.remove("root");
                        f.remove("preset");
                    },
                    window,
                    cx,
                );
                done
            }
            "layout.get" => {
                let path = self.ui_path.clone().ok_or("this window has no layout file")?;
                let text = std::fs::read_to_string(&path).unwrap_or_else(|_| "{}".into());
                serde_json::from_str(&text).map_err(|e| format!("ui.json isn't valid JSON: {e}"))
            }
            "layout.set" => {
                let json = args.get("json").ok_or("layout.set needs \"json\": the whole ui.json")?.clone();
                let path = self.ui_path.clone().ok_or("this window has no layout file")?;
                // checked before it is written: a broken layout never reaches the file.
                let text = serde_json::to_string_pretty(&json).map_err(|e| e.to_string())? + "\n";
                let parsed = crate::ui::parse(&text);
                let report = crate::ui::harmony::check_text(&parsed);
                parsed?;
                std::fs::write(&path, text).map_err(|e| format!("couldn't write {}: {e}", path.display()))?;
                self.poll_ui(window, cx);
                Ok(json!({"check": report}))
            }

            "ui.ls" => Ok(self.describe()),
            "commands.list" => Ok(Value::Array(
                crate::commands::COMMANDS
                    .iter()
                    .map(|s| json!({"id": s.id, "title": s.title, "keys": s.keys, "args": s.args, "preset": serde_json::from_str::<Value>(s.preset).unwrap_or_default()}))
                    .collect(),
            )),
            other => {
                let near: Vec<&str> = crate::commands::COMMANDS.iter().map(|s| s.id).filter(|id| id.split('.').next() == other.split('.').next()).collect();
                Err(if near.is_empty() { format!("there's no command \"{other}\". `commands.list` has them all") } else { format!("there's no command \"{other}\". did you mean one of: {}", { let mut n = near; n.dedup(); n.join(", ") }) })
            }
        };
        if out.is_ok() {
            self.publish("command_run", json!({"cmd": cmd}));
        }
        cx.notify();
        out
    }

    /// what the window shows, as json: `tesselctl ui ls`.
    pub(crate) fn describe(&self) -> Value {
        let focused = self.focused_pane().map(|p| p.id);
        let panes: Vec<Value> = self
            .ui
            .panes()
            .iter()
            .map(|p| {
                let rect = self.pane_rect(&p.id);
                json!({
                    "id": p.id,
                    "module": p.module.map(Module::name),
                    "open": self.pane_open(p),
                    "focused": focused.as_deref() == Some(p.id.as_str()),
                    "zoomed": self.pane_zoom.as_deref() == Some(p.id.as_str()),
                    "rect": rect,
                })
            })
            .collect();
        let drawers: Vec<Value> = self.drawers.iter().map(|d| json!({"id": d.id, "open": d.open, "width": d.size})).collect();
        let open = self.model.open_summary().map(|s| json!({"id": s.id.to_string(), "goal": s.goal, "state": format!("{:?}", s.run_state)}));
        let t = &self.ui.theme;
        json!({
            "session": open,
            "panes": panes,
            "drawers": drawers,
            "theme": {
                "base": match t.base.unwrap_or_default() { crate::ui::Base::Tessel => "tessel", crate::ui::Base::Orchard => "orchard", crate::ui::Base::Tern => "tern" },
                "dark": self.palette.get().dark,
                "size": t.scale.unwrap_or(1.0),
            },
            "sessions": self.model.sessions.len(),
            "layout_error": self.ui_error,
        })
    }

    /// a command from a key: what goes wrong shows as the window's error.
    pub(crate) fn run_key(&mut self, cmd: &str, args: Value, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.exec(cmd, &args, window, cx) {
            self.error = Some(e);
            cx.notify();
        }
    }

    /// the open session, or the one named in `args.session`.
    fn session_arg(&self, args: &Value) -> Result<SessionId, String> {
        match arg(args, "session") {
            Some(s) => self.find_session(s),
            None => self.model.open_summary().map(|s| s.id).ok_or_else(|| "no session is open; name one with \"session\"".into()),
        }
    }

    /// a session by `@open`, an id prefix, or part of its name.
    pub(crate) fn find_session(&self, what: &str) -> Result<SessionId, String> {
        if what == "@open" {
            return self.model.open_summary().map(|s| s.id).ok_or_else(|| "no session is open".into());
        }
        let by_id: Vec<&SessionSummary> = self.model.sessions.iter().filter(|s| s.id.to_string().starts_with(what)).collect();
        if by_id.len() == 1 {
            return Ok(by_id[0].id);
        }
        let lower = what.to_lowercase();
        let by_name: Vec<&SessionSummary> = self.model.sessions.iter().filter(|s| s.goal.to_lowercase().contains(&lower)).collect();
        match by_name.as_slice() {
            [one] => Ok(one.id),
            [] if by_id.is_empty() => Err(format!("no session matches \"{what}\"")),
            _ => Err(format!("\"{what}\" matches more than one session; use more of its id")),
        }
    }

    /// the focused pane, or the one named in `args.pane`.
    fn pane_arg(&self, args: &Value) -> Result<crate::ui::Pane, String> {
        match arg(args, "pane") {
            Some(p) => self.find_pane(p),
            None => self.focused_pane().ok_or_else(|| "this layout has no panes".into()),
        }
    }

    /// a pane by `@focused`, its id, or the module it shows.
    pub(crate) fn find_pane(&self, what: &str) -> Result<crate::ui::Pane, String> {
        if what == "@focused" {
            return self.focused_pane().ok_or_else(|| "this layout has no panes".into());
        }
        let panes = self.ui.panes();
        panes
            .iter()
            .find(|p| p.id == what)
            .or_else(|| panes.iter().find(|p| p.module.map(Module::name) == Some(what)))
            .map(|p| (*p).clone())
            .ok_or_else(|| format!("no pane \"{what}\". panes: {}", panes.iter().map(|p| p.id.as_str()).collect::<Vec<_>>().join(", ")))
    }

    /// tell whoever listens that something happened. builds nothing when nobody does.
    pub(crate) fn publish(&self, kind: &str, data: Value) {
        if self.ui_watched {
            self.cmd(Cmd::UiPublish { kind: kind.to_owned(), data });
        }
    }

    /// run what scripts asked for since the last frame, and answer each.
    pub(crate) fn run_ui_calls(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for (call, cmd, args) in std::mem::take(&mut self.ui_calls) {
            let args = if args.is_null() { json!({}) } else { args };
            let result = self.exec(&cmd, &args, window, cx);
            self.cmd(Cmd::UiAnswer { call, result });
        }
    }
}
