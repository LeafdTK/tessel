//! where a session starts: an existing folder, a GitHub repository, or a
//! new project made from a template.
//!
//! the + menu offers all three and the folders sessions have run in. making
//! a project (a folder, `git init`, a template, optionally a private GitHub
//! repository) happens on this machine, off the ui thread; then a session
//! starts in it. a template that needs more than a few commands (a Next.js
//! app) is handed to the agent as its first message instead of being run here.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sheet {
    /// a new project from a template.
    Create,
    /// a GitHub repository, cloned.
    Clone,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Template {
    #[default]
    Empty,
    Rust,
    Next,
}

/// id, template, name, what it is, lucide icon.
const TEMPLATES: [(&str, Template, &str, &str, &str); 3] = [
    ("empty", Template::Empty, "Empty", "A blank Git repository", "file-code"),
    ("rust", Template::Rust, "Rust", "cargo init, ready to build", "package"),
    ("next", Template::Next, "Next.js", "Set up by the agent: TypeScript, Tailwind", "app-window"),
];

/// a folder name made from what someone typed: lower case, dashes for spaces.
pub(crate) fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '_' || c == '.' {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_end_matches('-').to_owned()
}

/// `owner/repo`, `github.com/owner/repo` or a clone url, as something `git clone` takes, and the folder it makes.
pub(crate) fn clone_source(input: &str) -> Option<(String, String)> {
    let s = input.trim().trim_end_matches('/').trim_end_matches(".git");
    if s.is_empty() {
        return None;
    }
    let name = s.rsplit(['/', ':']).next()?.to_owned();
    if name.is_empty() {
        return None;
    }
    let url = if s.contains("://") || s.starts_with("git@") {
        format!("{s}.git")
    } else if let Some(rest) = s.strip_prefix("github.com/") {
        format!("https://github.com/{rest}.git")
    } else if s.split('/').count() == 2 {
        format!("https://github.com/{s}.git")
    } else {
        return None;
    };
    Some((url, name))
}

/// `~` for the home folder, so paths read short.
pub(crate) fn tilde(path: &str) -> String {
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && path.starts_with(&home) => format!("~{}", &path[home.len()..]),
        _ => path.to_owned(),
    }
}

fn untilde(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(rest),
        None => PathBuf::from(path),
    }
}

fn run(cmd: &mut Command, what: &str) -> Result<(), String> {
    let out = cmd.output().map_err(|e| format!("Couldn't run {what}: {e}."))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{what} failed: {}", err.lines().last().unwrap_or("").trim()))
    }
}

/// make the project's folder. what the session is first asked, if anything, comes back.
fn make_project(dir: &Path, name: &str, template: Template, github: bool) -> Result<String, String> {
    if dir.exists() && std::fs::read_dir(dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} already exists and isn't empty.", tilde(&dir.to_string_lossy())));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("Couldn't make {}: {e}.", dir.display()))?;
    let git = |args: &[&str]| run(Command::new("git").args(args).current_dir(dir), "git");
    match template {
        Template::Rust => run(Command::new("cargo").args(["init", "--vcs", "git", "--name", &slug(name).replace('.', "_")]).current_dir(dir), "cargo init")?,
        _ => {
            git(&["init", "-b", "main"])?;
            std::fs::write(dir.join("README.md"), format!("# {name}\n")).map_err(|e| format!("Couldn't write the readme: {e}."))?;
        }
    }
    // a first commit, so worktrees and branches have something to start from.
    // it can fail (no git identity set up); the project is still usable.
    let _ = git(&["add", "-A"]).and_then(|_| git(&["commit", "-q", "-m", "chore: start the project"]));
    if github {
        run(Command::new("gh").args(["repo", "create", &slug(name), "--private", "--source", ".", "--push"]).current_dir(dir), "gh repo create")?;
    }
    Ok(match template {
        Template::Next => "Set up a Next.js app in this empty repository with create-next-app: TypeScript, Tailwind and the App Router, in this folder rather than a new one. Then commit it.".into(),
        _ => String::new(),
    })
}

impl Shell {
    /// folders sessions have run in, the most recent first.
    pub(crate) fn recent_folders(&self, n: usize) -> Vec<String> {
        let mut sessions: Vec<&SessionSummary> = self.model.sessions.iter().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms.max(s.created_ms)));
        let mut out: Vec<String> = Vec::new();
        for s in sessions {
            if !out.contains(&s.cwd) {
                out.push(s.cwd.clone());
            }
        }
        out.truncate(n);
        out
    }

    /// the projects, as the sidebar has them: a folder each (a worktree's
    /// repository, not the worktree), the latest worked in first.
    pub(crate) fn project_list(&self) -> Vec<String> {
        let mut sessions: Vec<&SessionSummary> = self.model.sessions.iter().collect();
        sessions.sort_by_key(|s| std::cmp::Reverse(s.updated_ms.max(s.created_ms)));
        let mut out: Vec<String> = Vec::new();
        for s in sessions {
            let folder = s.worktree.as_ref().map_or(&s.cwd, |w| &w.repo);
            if !out.contains(folder) {
                out.push(folder.clone());
            }
        }
        out
    }

    /// where a new session would start: the project picked, or the latest.
    pub(crate) fn new_folder(&self) -> Option<String> {
        self.new_project.clone().or_else(|| self.project_list().into_iter().next())
    }

    /// the new-session card, pointed at a folder, with the cursor in what to do.
    pub(crate) fn start_in(&mut self, folder: String, window: &mut Window, cx: &mut Context<Self>) {
        let folder = match folder.strip_prefix("~/") {
            Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
            None => folder,
        };
        self.new_project = Some(folder);
        self.show_new = true;
        self.new_prompt.update(cx, |s, cx| s.focus(window, cx));
        cx.notify();
    }

    pub(crate) fn pick_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Open".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = super::in_window(&this, cx, |this, window, cx| this.start_in(path.to_string_lossy().into_owned(), window, cx));
        })
        .detach();
    }

    fn browse_parent(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = super::in_window(&this, cx, |this, window, cx| this.project_parent.update(cx, |s, cx| s.set_value(tilde(&path.to_string_lossy()), window, cx)));
        })
        .detach();
    }

    pub(crate) fn open_sheet(&mut self, sheet: Sheet, cx: &mut Context<Self>) {
        self.sheet = Some(sheet);
        self.sheet_error = None;
        self.focus_sheet = true;
        cx.notify();
    }

    pub fn close_sheet(&mut self, cx: &mut Context<Self>) {
        if !self.making {
            self.sheet = None;
        }
        cx.notify();
    }

    /// make the project (or clone it) off the ui thread, then start a session in it.
    pub(crate) fn make(&mut self, cx: &mut Context<Self>) {
        let Some(sheet) = self.sheet else { return };
        if self.making {
            return;
        }
        let parent = untilde(self.project_parent.read(cx).value().trim());
        if parent.as_os_str().is_empty() {
            self.sheet_error = Some("Choose a folder to put it in.".into());
            cx.notify();
            return;
        }
        let (template, github) = (self.template, self.github_private);
        let job: Box<dyn FnOnce() -> Result<(PathBuf, String), String> + Send> = match sheet {
            Sheet::Create => {
                let name = self.project_name.read(cx).value().trim().to_string();
                let folder = slug(&name);
                if folder.is_empty() {
                    self.sheet_error = Some("Give the project a name.".into());
                    cx.notify();
                    return;
                }
                let dir = parent.join(&folder);
                Box::new(move || make_project(&dir, &name, template, github).map(|prompt| (dir, prompt)))
            }
            Sheet::Clone => {
                let Some((url, name)) = clone_source(self.github_repo.read(cx).value().as_ref()) else {
                    self.sheet_error = Some("Write it as owner/repo, or paste its URL.".into());
                    cx.notify();
                    return;
                };
                let dir = parent.join(name);
                Box::new(move || {
                    if dir.exists() {
                        return Err(format!("{} already exists.", tilde(&dir.to_string_lossy())));
                    }
                    let _ = std::fs::create_dir_all(dir.parent().unwrap_or(&dir));
                    run(Command::new("git").arg("clone").arg(&url).arg(&dir), "git clone").map(|_| (dir, String::new()))
                })
            }
        };
        self.making = true;
        self.sheet_error = None;
        cx.notify();
        let work = cx.background_executor().spawn(async move { job() });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |this, cx| {
                this.making = false;
                match result {
                    Ok((dir, prompt)) => {
                        this.sheet = None;
                        this.cmd(Cmd::Create { cwd: dir.to_string_lossy().into_owned(), worktree: false, prompt, attachments: Vec::new() });
                    }
                    Err(e) => this.sheet_error = Some(e),
                }
                cx.notify();
            });
        })
        .detach();
    }

    // ---- drawing --------------------------------------------------------------------

    /// the ways to a project that isn't in the list yet. the + menu and the
    /// project picker offer the same three, so there's one way in.
    fn new_project_items(&self, list: Div, ids: &str, me: &WeakEntity<Shell>, popover: &Entity<gpui_kit::component::popover::PopoverState>) -> Div {
        let p = self.palette.get();
        let act = |f: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
            let (me, popover) = (me.clone(), popover.clone());
            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                popover.update(cx, |st, cx| st.dismiss(window, cx));
                let _ = me.update(cx, |this, cx| f(this, window, cx));
            }
        };
        list.child(theme::float_item(&p, SharedString::from(format!("{ids}-open")), Some("folder"), "Open project…", None, false).test_support().on_click(act(|this, w, cx| this.pick_folder(w, cx))))
            .child(theme::float_item(&p, SharedString::from(format!("{ids}-github")), Some("github"), "Open GitHub project…", None, false).test_support().on_click(act(|this, _, cx| this.open_sheet(Sheet::Clone, cx))))
            .child(theme::float_item(&p, SharedString::from(format!("{ids}-create")), Some("folder-plus"), "Quick start…", None, false).test_support().on_click(act(|this, _, cx| this.open_sheet(Sheet::Create, cx))))
    }

    /// which project a new session starts in: the sidebar's projects, then
    /// the ways to another.
    pub(crate) fn project_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let folder = self.new_folder();
        let name = folder.as_deref().map(folder_name);
        let me = cx.entity().downgrade();
        Popover::new("new-project-popover")
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(
                super::composer::tool_button("new-project")
                    .bg(p.fill)
                    .max_w(theme::z(280.))
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap(theme::z(8.))
                            .children(name.as_deref().map(|n| super::sidebar::project_tile(n, &p)))
                            .when(name.is_none(), |d| d.child(Icon::empty().path("icons/folder.svg").size(theme::z(15.)).text_color(p.secondary)))
                            .child(div().min_w_0().truncate().set(Type::Body).text_color(if name.is_some() { p.label } else { p.secondary }).child(name.clone().unwrap_or_else(|| "Choose a project".into())))
                            .child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(12.)).text_color(p.tertiary)),
                    ),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let current = this.new_folder();
                let mut list = theme::float_list(&p).w(px(320.));
                let projects = this.project_list();
                if !projects.is_empty() {
                    list = list.child(theme::float_label(&p, "Projects"));
                    let mut rows = v_flex().id("new-project-list").max_h(px(300.)).overflow_y_scroll();
                    for (i, folder) in projects.into_iter().enumerate() {
                        let on = current.as_deref() == Some(folder.as_str());
                        let name = folder_name(&folder);
                        let (me, popover, f) = (me.clone(), popover.clone(), folder.clone());
                        rows = rows.child(
                            h_flex()
                                .id(SharedString::from(format!("new-project-{i}")))
                                .test_support()
                                .aria_label(SharedString::from(format!("{name}{}", if on { ", chosen" } else { "" })))
                                .min_h(theme::z(44.))
                                .px(theme::z(10.))
                                .gap(theme::z(10.))
                                .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                                .cursor_pointer()
                                .when(on, |d| d.bg(p.selection))
                                .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                                .on_click(move |_, window, cx| {
                                    popover.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me.update(cx, |this, cx| {
                                        this.new_project = Some(f.clone());
                                        cx.notify();
                                    });
                                })
                                .child(super::sidebar::project_tile(&name, &p))
                                .child(
                                    v_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .child(div().set(Type::Body).text_color(p.label).child(name.clone()))
                                        .child(div().truncate().set(Type::Subheadline).text_color(p.tertiary).child(tilde(&folder))),
                                )
                                .when(on, |d| d.child(Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.accent))),
                        );
                    }
                    list = list.child(rows).child(theme::float_sep(&p));
                }
                this.new_project_items(list, "new-project", &me, &popover).into_any_element()
            })
            .into_any_element()
    }

    /// the + menu: open a folder, clone, start a project, or go back to a recent folder.
    pub(crate) fn add_menu(&self, id: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        Popover::new(SharedString::from(format!("{id}-popover")))
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(Button::new(id).ghost().small().icon(Icon::empty().path("icons/plus.svg")).tooltip("New Session"))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let recents = this.recent_folders(5);
                let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                let mut list = this.new_project_items(theme::float_list(&p).w(px(288.)), "add", &me, &popover);
                if !recents.is_empty() {
                    list = list.child(theme::float_sep(&p)).child(theme::float_label(&p, "Recents"));
                    for (i, folder) in recents.into_iter().enumerate() {
                        let f = folder.clone();
                        list = list.child(
                            theme::float_item(&p, SharedString::from(format!("recent-{i}")), Some("folder"), tilde(&folder), None, false)
                                .on_click(act(Box::new(move |this, w, cx| this.start_in(f.clone(), w, cx)))),
                        );
                    }
                }
                list.into_any_element()
            })
    }

    pub(crate) fn sheet_layer(&self, sheet: Sheet, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let label = |text: &'static str| div().set(Type::Callout).font_weight(FontWeight(550.)).child(text);
        let hint = |text: String| div().set(Type::Subheadline).text_color(p.secondary).child(text);
        let field = |input: &Entity<InputState>, id: &'static str| div().flex_1().child(Input::new(input).id(id));
        let (title, about) = match sheet {
            Sheet::Create => ("Create project", "A local folder with a Git repository, and a first session in it."),
            Sheet::Clone => ("Open GitHub project", "Clone a repository, and start a session in it."),
        };
        let parent = v_flex()
            .gap_2()
            .child(label("Parent folder"))
            .child(h_flex().gap_2().child(field(&self.project_parent, "project-parent")).child(Button::new("project-browse").outline().label("Browse").on_click(cx.listener(|this, _, window, cx| this.browse_parent(window, cx)))));

        let mut body = v_flex().gap_5();
        match sheet {
            Sheet::Create => {
                let name = self.project_name.read(cx).value().to_string();
                let folder = slug(&name);
                body = body.child(
                    v_flex().gap_2().child(label("Project name")).child(field(&self.project_name, "project-name")).when(!folder.is_empty(), |d| {
                        d.child(h_flex().gap_1().child(hint("Makes the folder".into())).child(div().px_1p5().rounded(px(theme::R_TILE / 2.0)).bg(p.fill).set(Type::Code).text_color(p.label).child(folder)))
                    }),
                );
                body = body.child(parent);
                let mut cards = h_flex().gap_3().items_stretch();
                for (id, t, name, what, icon) in TEMPLATES {
                    let on = self.template == t;
                    cards = cards.child(
                        v_flex()
                            .id(SharedString::from(format!("template-{id}")))
                            .test_support()
                            .aria_label(SharedString::from(format!("{name} template{}", if on { ", chosen" } else { "" })))
                            .flex_1()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .p_4()
                            .rounded(px(theme::R_CARD))
                            .border_1()
                            .border_color(if on { p.accent } else { p.separator })
                            .when(on, |d| d.bg(p.accent.opacity(0.06)))
                            .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.template = t;
                                cx.notify();
                            }))
                            .child(
                                div()
                                    .size(px(40.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(theme::R_CONTROL))
                                    .border_1()
                                    .border_color(p.separator)
                                    .bg(p.window)
                                    .text_color(p.label)
                                    .child(Icon::empty().path(format!("icons/{icon}.svg"))),
                            )
                            .child(div().set(Type::Body).font_weight(FontWeight(550.)).child(name))
                            .child(div().w_full().text_center().set(Type::Subheadline).text_color(p.secondary).child(what)),
                    );
                }
                body = body.child(v_flex().gap_2().child(label("Template")).child(cards));
                let me = cx.entity().downgrade();
                body = body.child(
                    h_flex()
                        .gap_3()
                        .child(gpui_kit::component::switch::Switch::new("project-github").checked(self.github_private).on_click(move |on, _, cx| {
                            let on = *on;
                            let _ = me.update(cx, |this, cx| {
                                this.github_private = on;
                                cx.notify();
                            });
                        }))
                        .child(v_flex().child(div().set(Type::Body).child("Also make a private GitHub repository")).child(hint("With the gh command, signed in as you.".into()))),
                );
            }
            Sheet::Clone => {
                let source = clone_source(self.github_repo.read(cx).value().as_ref());
                body = body.child(
                    v_flex().gap_2().child(label("Repository")).child(field(&self.github_repo, "github-repo")).child(match source {
                        Some((_, name)) => h_flex().gap_1().child(hint("Clones into".into())).child(div().px_1p5().rounded(px(theme::R_TILE / 2.0)).bg(p.fill).set(Type::Code).text_color(p.label).child(name)).into_any_element(),
                        None => hint("owner/repo, or the repository's URL".into()).into_any_element(),
                    }),
                );
                body = body.child(parent);
            }
        }

        div()
            .id("sheet-layer")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.sheet_focus)
            .key_context("Sheet")
            .on_action(cx.listener(|this, _: &crate::CloseSheet, _, cx| this.close_sheet(cx)))
            .on_action(cx.listener(|this, _: &crate::ConfirmSheet, _, cx| this.make(cx)))
            .bg(gpui_kit::black().opacity(if p.dark { 0.45 } else { 0.18 }))
            .on_click(cx.listener(|this, _, _, cx| this.close_sheet(cx)))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .id("project-sheet")
                    .test_support()
                    .aria_label(title)
                    .w(px(600.))
                    .max_w(relative(0.92))
                    .p_6()
                    .gap_5()
                    .rounded(px(theme::R_PANEL))
                    .border_1()
                    .border_color(p.separator)
                    .bg(p.raised)
                    .shadow_lg()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(v_flex().gap_1().child(div().text_size(px(20.)).line_height(px(26.)).font_weight(FontWeight(650.)).child(title)).child(hint(about.into())))
                    .child(body)
                    .children(self.sheet_error.clone().map(|e| div().set(Type::Callout).text_color(p.failed).child(e)))
                    .child(
                        h_flex()
                            .gap_2()
                            .justify_end()
                            .child(Button::new("project-cancel").ghost().label("Cancel").on_click(cx.listener(|this, _, _, cx| this.close_sheet(cx))))
                            .child(
                                Button::new("project-make")
                                    .primary()
                                    .label(match (sheet, self.making) {
                                        (_, true) => "Working…",
                                        (Sheet::Create, false) => "Create  ⌘↵",
                                        (Sheet::Clone, false) => "Clone  ⌘↵",
                                    })
                                    .on_click(cx.listener(|this, _, _, cx| this.make(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::{Template, clone_source, make_project, slug};

    #[test]
    fn names_become_folder_names() {
        assert_eq!(slug("Starter Playground"), "starter-playground");
        assert_eq!(slug("  my app!! v2 "), "my-app-v2");
        assert_eq!(slug("tessel.rs"), "tessel.rs");
        assert_eq!(slug("???"), "");
    }

    #[test]
    fn repositories_are_read_however_they_are_written() {
        assert_eq!(clone_source("hackclub/orchard"), Some(("https://github.com/hackclub/orchard.git".into(), "orchard".into())));
        assert_eq!(clone_source("github.com/hackclub/orchard/"), Some(("https://github.com/hackclub/orchard.git".into(), "orchard".into())));
        assert_eq!(clone_source("https://github.com/hackclub/orchard.git"), Some(("https://github.com/hackclub/orchard.git".into(), "orchard".into())));
        assert_eq!(clone_source("git@github.com:hackclub/orchard.git"), Some(("git@github.com:hackclub/orchard.git".into(), "orchard".into())));
        assert_eq!(clone_source("orchard"), None);
        assert_eq!(clone_source(""), None);
    }

    #[test]
    fn an_empty_project_is_a_git_repository_with_a_readme_and_never_overwrites() {
        let dir = std::env::temp_dir().join(format!("tessel-project-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let first = make_project(&dir, "My App", Template::Empty, false).unwrap();
        assert!(first.is_empty(), "an empty project starts with nothing to do");
        assert!(dir.join(".git").is_dir() && std::fs::read_to_string(dir.join("README.md")).unwrap() == "# My App\n");
        assert!(make_project(&dir, "My App", Template::Empty, false).unwrap_err().contains("isn't empty"));
        std::fs::remove_dir_all(&dir).unwrap();
        let next = std::env::temp_dir().join(format!("tessel-project-next-{}", std::process::id()));
        let ask = make_project(&next, "Site", Template::Next, false).unwrap();
        assert!(ask.contains("create-next-app"), "next.js is the agent's first job: {ask}");
        std::fs::remove_dir_all(&next).unwrap();
    }
}

/// a project's name: its folder's.
pub(crate) fn folder_name(folder: &str) -> String {
    folder.trim_end_matches('/').rsplit('/').next().filter(|n| !n.is_empty()).unwrap_or("/").to_owned()
}
