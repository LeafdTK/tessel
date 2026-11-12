//! a workspace's scripts, as buttons: the run button in the conversation's
//! header starts the default one (the dev server, usually) in a terminal pane
//! and stops it again; its menu has the rest, the setup again, and a way to
//! have an agent write them.
//!
//! what a workspace's scripts are is the daemon's to say (`Request::Scripts`,
//! read from `.tessel/settings.toml` or a Conductor file); running one is
//! opening a terminal with it, so it shows, keeps running with the window shut,
//! and is stopped by closing that terminal.

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

Commands run in a non-interactive shell from the workspace folder, so put any toolchain setup in the command itself. If there's a `.conductor/settings.toml` or `conductor.json`, start from it. Don't commit the file. When it's written, say what each script does in a line each.";

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
        self.cmd(Cmd::Create { cwd, worktree: false, prompt: SETUP_PROMPT.to_owned() });
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
                        let label = if s.from.is_some() { "Change scripts with an agent" } else { "Set up scripts with an agent" };
                        list = list.child(theme::float_item(&p, "run-ai", Some("sparkles"), label, Some("A new chat that writes .tessel/settings.toml"), false).test_support().on_click(act(Box::new(|this, _, cx| this.setup_scripts_with_ai(cx)))));
                        if let Some(from) = s.from.clone() {
                            list = list.child(theme::float_item(&p, "run-edit", Some("file-cog"), "Edit the settings file", Some(&from), false).test_support().on_click(act(Box::new(move |this, _, cx| this.open_file(from.clone(), cx)))));
                        }
                        list = list.child(div().px(px(10.)).pt(px(4.)).pb(px(6.)).set(Type::Subheadline).text_color(p.tertiary).child(format!("Ports {} to {}, as $TESSEL_PORT", port, port + 9)));
                    }
                }
                list.into_any_element()
            });
        Some(h_flex().gap(px(1.)).items_center().child(main).child(menu).when(running, |d| d.child(super::live_dot(p.blocked, &p))).into_any_element())
    }
}
