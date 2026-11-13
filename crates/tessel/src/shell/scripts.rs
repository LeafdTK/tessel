//! a workspace's scripts, as buttons: the run button in the conversation's
//! header starts the default one (the dev server, usually) in a terminal pane
//! and stops it again; its menu has the rest, the setup again, and a way to
//! have an agent write them.
//!
//! what a workspace's scripts are is the daemon's to say (`Request::Scripts`,
//! read from `.tessel/settings.toml` and this machine's `settings.local.toml`);
//! running one is opening a terminal with it, so it shows, keeps running with
//! the window shut, and is stopped by closing that terminal. Settings has a
//! page for writing them by hand.

use super::Shell;
use crate::bridge::Cmd;
use crate::theme::{self, Type, Typeset as _};
use crate::ui::Module;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tessel_proto::term::TermId;
use tessel_proto::{Request, WorkspaceScripts};

/// what the agent asked to set up a workspace's scripts is told.
pub(crate) const SETUP_PROMPT: &str = "Set up this repository's workspace scripts for Tessel. Look at how the project is installed, built, run and tested, then write `.tessel/settings.toml`:

```toml
[scripts]
setup = \"...\"      # runs once in each new workspace: install dependencies, generate files

[scripts.run.dev]    # one table per button; its name is the button's label
command = \"...\"    # serve on $TESSEL_PORT (the workspace has $TESSEL_PORT to $TESSEL_PORT+9), never a fixed port
default = true       # the one the run button starts
icon = \"play\"      # a lucide icon name

[files]
copy = [\".env*\"]   # gitignored files each new workspace needs from the main checkout
```

Add `archive = \"...\"` under [scripts] if something should be shut down or tidied when a workspace is removed, and `run_mode = \"nonconcurrent\"` if a script can only run in one workspace at a time (a fixed port, a shared database). Commands run in a non-interactive shell from the workspace folder, so put any toolchain setup in the command itself. Don't commit the file. When it's written, say what each script does in a line each.";

impl Shell {
    /// the open chat's workspace and its scripts, once the daemon has said.
    pub(crate) fn open_scripts(&self) -> Option<&(WorkspaceScripts, u16)> {
        let cwd = &self.model.open_summary()?.cwd;
        self.scripts.get(cwd)
    }

    /// ask the daemon what the open chat's workspace has, once; `again`: even if known.
    pub(crate) fn ask_scripts(&mut self, again: bool) {
        let Some(s) = self.model.open_summary() else { return };
        let (id, cwd) = (s.id, s.cwd.clone());
        if self.scripts_asked.is_some() || (!again && (self.scripts.contains_key(&cwd) || self.scripts_tried.contains(&cwd))) {
            return;
        }
        self.scripts_tried.insert(cwd.clone());
        self.scripts_asked = Some(cwd);
        self.cmd(Cmd::Ask { tag: "scripts", req: Request::Scripts { session: id } });
    }

    pub(crate) fn scripts_answer(&mut self, result: Result<tessel_proto::Response, String>) {
        let Some(cwd) = self.scripts_asked.take() else { return };
        if let Ok(tessel_proto::Response::Scripts { scripts, port }) = result {
            self.scripts.insert(cwd, (scripts, port));
        }
    }

    pub(crate) fn running_script(&self, command: &str) -> Option<TermId> {
        self.running(command)
    }

    /// the open chat's terminal running `command`, if one is.
    fn running(&self, command: &str) -> Option<TermId> {
        let session = self.model.open.as_ref().map(|o| o.id)?;
        self.terms.iter().find(|(_, v)| {
            let v = v.borrow();
            v.info.session == Some(session) && v.info.exited.is_none() && v.info.command.len() == 1 && v.info.command[0] == command
        }).map(|(id, _)| *id)
    }

    /// a terminal pane to show a script in: the first open one, or a new one below.
    fn script_pane(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        let open = self.ui.panes().into_iter().find(|p| p.module == Some(Module::Terminal) && self.pane_open(p)).map(|p| p.id.clone());
        open.or_else(|| self.new_terminal(window, cx))
    }

    /// run a command in the open chat's workspace, shown in a terminal pane.
    /// already running: show it.
    pub(crate) fn run_script(&mut self, command: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else { return };
        let Some(pane) = self.script_pane(window, cx) else { return };
        // one workspace at a time: starting it here stops it in the repository's others.
        if self.open_scripts().is_some_and(|(s, _)| !s.concurrent) {
            let repo = self.model.open_summary().map(crate::model::project_name);
            let elsewhere: Vec<TermId> = self
                .terms
                .iter()
                .filter(|(_, v)| {
                    let v = v.borrow();
                    v.info.exited.is_none()
                        && v.info.command == [command.clone()]
                        && v.info.session.is_some_and(|s| s != session && self.model.summary(s).map(crate::model::project_name) == repo)
                })
                .map(|(id, _)| *id)
                .collect();
            for id in elsewhere {
                self.cmd(Cmd::TermClose(id));
            }
        }
        match self.running(&command) {
            Some(id) => {
                self.pane_terms.insert((pane, Some(session)), id);
            }
            None => {
                self.term_opening.insert((pane.clone(), Some(session)));
                self.cmd(Cmd::TermOpen { pane, session: Some(session), cwd: None, command: vec![command], cols: 100, rows: 30 });
            }
        }
        cx.notify();
    }

    /// the run button: the default script, started or stopped.
    pub(crate) fn toggle_default_script(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(action) = self.open_scripts().and_then(|(s, _)| s.actions.iter().find(|a| a.default).cloned()) else {
            self.setup_scripts_with_ai(cx);
            return;
        };
        match self.running(&action.command) {
            Some(id) => self.cmd(Cmd::TermClose(id)),
            None => self.run_script(action.command, window, cx),
        }
        cx.notify();
    }

    /// a new chat in this workspace whose job is to write its scripts.
    pub(crate) fn setup_scripts_with_ai(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.model.open_summary().map(|s| s.cwd.clone()) else { return };
        self.show_new = false;
        self.cmd(Cmd::Create { cwd, worktree: false, prompt: SETUP_PROMPT.to_owned(), attachments: Vec::new() });
        // what it writes shows up the next time the menu is opened.
        self.scripts.remove(&self.model.open_summary().map(|s| s.cwd.clone()).unwrap_or_default());
        cx.notify();
    }

    /// the run button and its menu, in the conversation's header.
    pub(crate) fn run_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        self.model.open_summary()?;
        if self.show_new {
            return None;
        }
        let p = self.palette.get();
        let known = self.open_scripts();
        let default = known.and_then(|(s, _)| s.actions.iter().find(|a| a.default));
        let running = default.and_then(|a| self.running(&a.command)).is_some();
        let main = match default {
            Some(a) => Button::new("run-default")
                .outline()
                .small()
                .icon(Icon::empty().path(if running { "icons/square.svg" } else { "icons/play.svg" }))
                .label(if running { format!("Stop {}", a.id) } else { a.id.clone() })
                .tooltip(if running { "Stop it" } else { "Run it in a terminal" }),
            None => Button::new("run-default").ghost().small().icon(Icon::empty().path("icons/play.svg")).tooltip("Set up run scripts for this workspace"),
        }
        .on_click(cx.listener(|this, _, window, cx| this.toggle_default_script(window, cx)));
        let me = cx.entity().downgrade();
        let menu = Popover::new("run-menu")
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(Button::new("run-more").ghost().small().icon(Icon::empty().path("icons/chevron-down.svg")).tooltip("Scripts"))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                let mut list = theme::float_list(&p).w(px(300.));
                let scripts = this.open_scripts().cloned();
                match &scripts {
                    None => list = list.child(div().px(px(10.)).py(px(6.)).set(Type::Callout).text_color(p.secondary).child("Looking…")),
                    Some((s, port)) => {
                        if s.actions.is_empty() {
                            list = list.child(div().px(px(10.)).py(px(6.)).set(Type::Callout).text_color(p.secondary).child("No run scripts yet."));
                        }
                        for (i, a) in s.actions.iter().enumerate() {
                            let on = this.running(&a.command).is_some();
                            let command = a.command.clone();
                            let icon = if on { "square" } else { a.icon.as_deref().unwrap_or("play") };
                            let title = if on { format!("Stop {}", a.id) } else { a.id.clone() };
                            list = list.child(
                                theme::float_item(&p, SharedString::from(format!("run-action-{i}")), Some(icon), &title, Some(&a.command), false).test_support().on_click(act(Box::new(move |this, window, cx| {
                                    match this.running(&command) {
                                        Some(id) => this.cmd(Cmd::TermClose(id)),
                                        None => this.run_script(command.clone(), window, cx),
                                    }
                                }))),
                            );
                        }
                        list = list.child(theme::float_sep(&p));
                        if let Some(setup) = s.setup.clone() {
                            list = list.child(theme::float_item(&p, "run-setup", Some("rotate-ccw"), "Run setup again", Some(&setup), false).test_support().on_click(act(Box::new(move |this, window, cx| this.run_script(setup.clone(), window, cx)))));
                        }
                        let label = if s.from.is_empty() { "Set up scripts with an agent" } else { "Change scripts with an agent" };
                        list = list.child(theme::float_item(&p, "run-ai", Some("sparkles"), label, Some("A new chat that writes .tessel/settings.toml"), false).test_support().on_click(act(Box::new(|this, _, cx| this.setup_scripts_with_ai(cx)))));
                        list = list.child(theme::float_item(&p, "run-edit", Some("settings-2"), "Edit scripts", Some("Settings, by hand"), false).test_support().on_click(act(Box::new(|this, _, cx| {
                            this.settings_section = "scripts";
                            this.open_settings(cx);
                        }))));
                        list = list.child(div().px(px(10.)).pt(px(4.)).pb(px(6.)).set(Type::Subheadline).text_color(p.tertiary).child(format!("Ports {} to {}, as $TESSEL_PORT", port, port + 9)));
                    }
                }
                list.into_any_element()
            });
        Some(h_flex().gap(px(1.)).items_center().child(main).child(menu).when(running, |d| d.child(super::live_dot(p.blocked, &p))).into_any_element())
    }
}


/// the Scripts page's fields, for the workspace whose scripts they are.
pub(crate) struct ScriptsForm {
    pub(crate) cwd: String,
    pub(crate) session: tessel_proto::SessionId,
    pub(crate) setup: Entity<gpui_kit::component::input::InputState>,
    pub(crate) archive: Entity<gpui_kit::component::input::InputState>,
    pub(crate) copy: Entity<gpui_kit::component::input::InputState>,
    pub(crate) rows: Vec<ScriptRow>,
    pub(crate) concurrent: bool,
    /// what saving last said.
    pub(crate) said: Option<String>,
}

pub(crate) struct ScriptRow {
    pub(crate) name: Entity<gpui_kit::component::input::InputState>,
    pub(crate) command: Entity<gpui_kit::component::input::InputState>,
    pub(crate) icon: Option<String>,
    pub(crate) default: bool,
}

impl Shell {
    fn field(value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Entity<gpui_kit::component::input::InputState> {
        let value = value.to_owned();
        cx.new(|cx| {
            let mut s = gpui_kit::component::input::InputState::new(window, cx).placeholder(placeholder);
            s.set_value(value, window, cx);
            s
        })
    }

    fn script_row(name: &str, command: &str, icon: Option<String>, default: bool, window: &mut Window, cx: &mut Context<Self>) -> ScriptRow {
        ScriptRow { name: Self::field(name, "dev", window, cx), command: Self::field(command, "pnpm dev --port $TESSEL_PORT", window, cx), icon, default }
    }

    /// the page's fields, filled from what the open workspace has, made when
    /// the page is first shown for it (inputs need the window).
    pub(crate) fn ensure_scripts_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.settings_open || self.settings_section != "scripts" {
            return;
        }
        let Some(summary) = self.model.open_summary() else { return };
        let (cwd, session) = (summary.cwd.clone(), summary.id);
        if self.scripts_form.as_ref().is_some_and(|f| f.cwd == cwd) {
            return;
        }
        let Some((s, _)) = self.open_scripts().cloned() else {
            self.ask_scripts(false);
            return;
        };
        let rows = s.actions.iter().map(|a| Self::script_row(&a.id, &a.command, a.icon.clone(), a.default, window, cx)).collect();
        self.scripts_form = Some(ScriptsForm {
            cwd,
            session,
            setup: Self::field(s.setup.as_deref().unwrap_or(""), "pnpm install", window, cx),
            archive: Self::field(s.archive.as_deref().unwrap_or(""), "docker compose down", window, cx),
            copy: Self::field(&s.copy.join(", "), ".env*", window, cx),
            rows,
            concurrent: s.concurrent,
            said: None,
        });
    }

    /// what the page says, as settings.
    fn form_scripts(&self, f: &ScriptsForm, cx: &App) -> WorkspaceScripts {
        let text = |e: &Entity<gpui_kit::component::input::InputState>| Some(e.read(cx).value().trim().to_owned()).filter(|t| !t.is_empty());
        WorkspaceScripts {
            setup: text(&f.setup),
            archive: text(&f.archive),
            actions: f
                .rows
                .iter()
                .filter_map(|r| Some(tessel_proto::ScriptAction { id: text(&r.name)?, command: text(&r.command)?, icon: r.icon.clone(), default: r.default }))
                .collect(),
            copy: f.copy.read(cx).value().split([',', ' ', '\n']).map(str::trim).filter(|g| !g.is_empty()).map(str::to_owned).collect(),
            concurrent: f.concurrent,
            from: Vec::new(),
        }
    }

    /// write the page: for the team (the committed file) or for this Mac only.
    pub(crate) fn save_scripts(&mut self, local: bool, cx: &mut Context<Self>) {
        let Some(f) = self.scripts_form.as_ref() else { return };
        let (scripts, session, cwd) = (self.form_scripts(f, cx), f.session, f.cwd.clone());
        if self.scripts_asked.is_some() {
            return;
        }
        self.scripts_asked = Some(cwd);
        self.cmd(Cmd::Ask { tag: "scripts", req: Request::ScriptsSave { session, scripts, local } });
        if let Some(f) = self.scripts_form.as_mut() {
            f.said = Some(if local { "Saved for this Mac, in the main checkout's .tessel/settings.local.toml.".into() } else { "Saved in this workspace's .tessel/settings.toml. Commit it to share it.".into() });
        }
        cx.notify();
    }

    /// Settings → Scripts.
    pub(crate) fn scripts_settings(&self, cx: &mut Context<Self>) -> gpui_kit::Div {
        use gpui_kit::component::input::Input;
        use gpui_kit::component::v_flex;
        let p = self.palette.get();
        let note = |t: String| div().px_4().py_3().set(Type::Callout).text_color(p.secondary).child(t);
        let Some(summary) = self.model.open_summary() else {
            return v_flex().child(note("Open a chat in a repository to set up its scripts.".into()));
        };
        let Some(f) = self.scripts_form.as_ref().filter(|f| f.cwd == summary.cwd) else {
            return v_flex().child(note("Reading this workspace's scripts…".into()));
        };
        let port = self.open_scripts().map_or(0, |(_, p)| *p);
        let input = |e: &Entity<gpui_kit::component::input::InputState>, id: String| Input::new(e).id(SharedString::from(id)).small();
        let mut rows = v_flex().gap(px(6.)).w(px(440.));
        for (i, r) in f.rows.iter().enumerate() {
            rows = rows.child(
                h_flex()
                    .gap(px(6.))
                    .items_center()
                    .child(div().w(px(90.)).child(input(&r.name, format!("script-{i}-name"))))
                    .child(div().flex_1().min_w_0().child(input(&r.command, format!("script-{i}-command"))))
                    .child(
                        Button::new(SharedString::from(format!("script-{i}-default")))
                            .ghost()
                            .small()
                            .icon(Icon::empty().path(if r.default { "icons/circle-check.svg" } else { "icons/circle.svg" }))
                            .tooltip("The run button starts this one")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(f) = this.scripts_form.as_mut() {
                                    for (j, r) in f.rows.iter_mut().enumerate() {
                                        r.default = j == i;
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(Button::new(SharedString::from(format!("script-{i}-remove"))).ghost().small().icon(Icon::empty().path("icons/x.svg")).tooltip("Remove").on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(f) = this.scripts_form.as_mut()
                            && i < f.rows.len()
                        {
                            f.rows.remove(i);
                        }
                        cx.notify();
                    }))),
            );
        }
        rows = rows.child(h_flex().child(Button::new("script-add").ghost().small().icon(Icon::empty().path("icons/plus.svg")).label("Add a script").on_click(cx.listener(|this, _, window, cx| {
            let first = this.scripts_form.as_ref().is_some_and(|f| f.rows.is_empty());
            let row = Self::script_row("", "", None, first, window, cx);
            if let Some(f) = this.scripts_form.as_mut() {
                f.rows.push(row);
            }
            cx.notify();
        }))));
        let mode = if f.concurrent { "together" } else { "alone" };
        v_flex()
            .child(note(format!(
                "What every workspace of {} gets. Saved for the team, it's .tessel/settings.toml in the repository; saved for this Mac, .tessel/settings.local.toml beside it, which wins. Each workspace has ports {port} to {}, as $TESSEL_PORT.",
                crate::model::project_name(summary),
                port + 9
            )))
            .child(self.setting_row("Setup", "Runs once in each new workspace, in a terminal you can watch.", div().w(px(300.)).child(input(&f.setup, "scripts-setup".into()))))
            .child(
                v_flex()
                    .px_4()
                    .py_2()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.separator)
                    .child(v_flex().gap(px(2.)).child(div().set(Type::Body).child("Run scripts")).child(div().set(Type::Subheadline).text_color(p.secondary).child("Each is a button; the ticked one is the run button.")))
                    .child(rows),
            )
            .child(self.setting_row("Archive", "Runs when a workspace is removed: shut down what it started.", div().w(px(300.)).child(input(&f.archive, "scripts-archive".into()))))
            .child(self.setting_row("Files to copy", "Ignored files each new workspace gets from the main checkout. Separate them with commas.", div().w(px(300.)).child(input(&f.copy, "scripts-copy".into()))))
            .child(self.setting_row(
                "Run mode",
                "Together: every workspace can run them at once, each on its own ports. Alone: starting one stops it in the others.",
                self.segmented("scripts-mode", &[("together", "Together"), ("alone", "Alone")], mode, cx, |this, v, _, cx| {
                    if let Some(f) = this.scripts_form.as_mut() {
                        f.concurrent = v == "together";
                    }
                    cx.notify();
                }),
            ))
            .child(
                h_flex()
                    .px_4()
                    .py_3()
                    .gap(px(8.))
                    .border_t_1()
                    .border_color(p.separator)
                    .items_center()
                    .child(Button::new("scripts-save-team").primary().small().label("Save for the team").on_click(cx.listener(|this, _, _, cx| this.save_scripts(false, cx))))
                    .child(Button::new("scripts-save-local").outline().small().label("Save for this Mac").on_click(cx.listener(|this, _, _, cx| this.save_scripts(true, cx))))
                    .child(div().flex_1())
                    .child(Button::new("scripts-ai").ghost().small().icon(Icon::empty().path("icons/sparkles.svg")).label("Set up with an agent").on_click(cx.listener(|this, _, _, cx| {
                        this.settings_open = false;
                        this.setup_scripts_with_ai(cx);
                    }))),
            )
            .children(f.said.clone().map(note))
    }
}
