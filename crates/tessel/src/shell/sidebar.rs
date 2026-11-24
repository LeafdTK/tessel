//! what the sidebar lets you do with a project or a session: start another
//! session there, open its folder, rename it, fold a project away, delete.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;

impl Shell {
    /// the folder a project's sessions run in: its repository for worktrees.
    pub(crate) fn project_folder(&self, project: &str) -> Option<String> {
        self.model
            .sessions
            .iter()
            .find(|s| project_name(s) == project)
            .map(|s| s.worktree.as_ref().map_or(s.cwd.clone(), |w| w.repo.clone()))
            .or_else(|| self.model.projects.iter().find(|f| crate::model::folder_name(f) == project).cloned())
    }

    /// every project a session runs in is remembered by the machine, so it stays
    /// in the list after its last session goes. not on a daemon too old to keep them.
    pub(crate) fn remember_projects(&mut self) {
        let Some(known) = self.daemon_settings.as_ref().and_then(|d| d.projects.clone()) else { return };
        // a tab opened in a worktree may not know it is one: its workspace's other tabs do.
        let repo_of: std::collections::HashMap<&str, &str> = self.model.sessions.iter().filter_map(|s| s.worktree.as_ref().map(|w| (s.cwd.as_str(), w.repo.as_str()))).collect();
        let mut next = known.clone();
        for s in &self.model.sessions {
            let folder = s.worktree.as_ref().map(|w| w.repo.as_str()).or_else(|| repo_of.get(s.cwd.as_str()).copied()).unwrap_or(s.cwd.as_str());
            let folder = folder.trim_end_matches('/');
            if !folder.is_empty() && !next.iter().any(|f| f == folder) {
                next.push(folder.to_owned());
            }
        }
        if next != known {
            self.set_projects(next);
        }
    }

    fn set_projects(&mut self, projects: Vec<String>) {
        self.model.projects = projects.clone();
        if let Some(d) = self.daemon_settings.as_mut() {
            d.projects = Some(projects.clone());
        }
        self.cmd(Cmd::SetSettings(serde_json::json!({ "projects": projects })));
    }

    /// take a project with no sessions off the list.
    pub(crate) fn forget_project(&mut self, folder: &str, cx: &mut Context<Self>) {
        let next: Vec<String> = self.model.projects.iter().filter(|f| *f != folder).cloned().collect();
        self.set_projects(next);
        self.reflow_sidebar();
        cx.notify();
    }

    pub(crate) fn toggle_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if !self.model.collapsed.remove(project) {
            self.model.collapsed.insert(project.to_owned());
        }
        // a fold is yours: it happens now, even with the pointer over the list,
        // and its rows are simply there, with no slide to bounce.
        self.reflow_sidebar_as(false, false);
        cx.notify();
    }

    /// a project's heading: its tile, its name and how many sessions it has;
    /// click to fold it, and on hover a new session and a menu.
    pub(crate) fn project_header(&self, title: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let queue = title == crate::model::QUEUE_TITLE;
        let folded = self.model.collapsed.contains(title);
        let group = SharedString::from(format!("project-{title}"));
        let name = title.to_owned();
        let count = if queue { self.model.attention_queue().len() } else { self.model.sessions.iter().filter(|s| project_name(s) == title && s.attention.is_none()).count() };
        let tile = if queue {
            div().size(theme::z(16.)).flex_shrink_0().flex().items_center().justify_center().rounded(theme::z(4.)).bg(theme::info(&p).opacity(0.16)).child(Icon::empty().path("icons/bell.svg").size(theme::z(10.)).text_color(theme::info(&p))).into_any_element()
        } else {
            super::avatar::letter(title, 18., 5., &p).into_any_element()
        };
        let mut row = h_flex()
            .id(SharedString::from(format!("project-{title}")))
            .test_support()
            .group(group.clone())
            // a little air above and below, so a hovered header never touches a selected row.
            .h(px(crate::model::HEADER_H - 3.))
            .my(px(1.5))
            .mx_2()
            .pl(px(8.))
            .pr(px(8.))
            // the name starts where the chats' titles do: one line down the list.
            .gap(px(4.))
            .items_center()
            .rounded(px(theme::R_TILE))
            // liquid: each project's header is a strip of glass over the desktop.
            .when(crate::glass::liquid(), |d| d.relative().child(super::glass_spot(theme::R_TILE)))
            .when(!queue, |d| {
                let n = name.clone();
                d.hover(|d| d.bg(p.hover)).on_click(cx.listener(move |this, _, _, cx| this.toggle_project(&n, cx)))
            })
            .child(tile)
            .child(div().min_w_0().truncate().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(title.to_owned()))
            // which way it's folded, said quietly: there when folded, or under the pointer.
            .child(
                div()
                    .flex_1()
                    .when(!queue, |d| {
                        d.child(
                            div()
                                .when(!folded, |i| i.invisible().group_hover(group.clone(), |s| s.visible()))
                                .child(Icon::empty().path(if folded { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" }).size(theme::z(11.)).text_color(p.tertiary)),
                        )
                    }),
            );
        // folded, how many it hides; open, the chats themselves say.
        let count = if folded || queue { count } else { 0 };
        // the count, and on hover the + and ⋯ in its place: one slot at the
        // row's end, lined up with the counts and keys above it.
        let pill = (count > 0).then(|| theme::count_pill(&p, count.to_string(), queue));
        if !queue && let Some(folder) = self.project_folder(title) {
            let (f1, me) = (folder.clone(), cx.entity().downgrade());
            let controls = h_flex()
                .gap_0p5()
                .flex_shrink_0()
                .absolute()
                .right_0()
                .top_0()
                .bottom_0()
                .items_center()
                .invisible()
                .group_hover(group.clone(), |s| s.visible())
                .child(Button::new(SharedString::from(format!("project-{title}-new"))).ghost().small().rounded(px(5.5)).icon(Icon::empty().path("icons/plus.svg")).tooltip("New Session Here").on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    // a workspace of its own unless asked otherwise (a plain folder can't have one).
                    this.new_worktree = true;
                    this.start_in(f1.clone(), window, cx);
                })))
                .child(
                    // the menu's clicks stay the menu's: the row underneath folds the project on a click.
                    div().id(SharedString::from(format!("project-{title}-menu-zone"))).on_click(|_, _, cx| cx.stop_propagation()).child(
                    Popover::new(SharedString::from(format!("project-{title}-menu")))
                        .p_0()
                        .anchor(Anchor::TopRight)
                        .trigger(Button::new(SharedString::from(format!("project-{title}-more"))).ghost().small().rounded(px(5.5)).icon(Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
                        .content(move |_, _, cx| {
                            let popover = cx.entity();
                            let Some(shell) = me.upgrade() else { return div().into_any_element() };
                            let p = shell.read(cx).palette.get();
                            let folded = shell.read(cx).model.collapsed.contains(&name);
                            let empty = !shell.read(cx).model.sessions.iter().any(|s| project_name(s) == name);
                            let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>| {
                                let (me, popover) = (me.clone(), popover.clone());
                                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    popover.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me.update(cx, |this, cx| f(this, window, cx));
                                }
                            };
                            let (f1, f2, f3, f4, f5, n) = (folder.clone(), folder.clone(), folder.clone(), folder.clone(), folder.clone(), name.clone());
                            theme::float_list(&p)
                                .w(px(240.))
                                .child(theme::float_item(&p, "project-menu-new", Some("plus"), "New session, in place", None, false).on_click(act(Box::new(move |this, w, cx| {
                                    this.new_worktree = false;
                                    this.start_in(f1.clone(), w, cx);
                                }))))
                                .child(theme::float_item(&p, "project-menu-worktree", Some("git-branch"), "New session in a worktree", None, false).on_click(act(Box::new(move |this, w, cx| {
                                    this.new_worktree = true;
                                    this.start_in(f2.clone(), w, cx);
                                }))))
                                .child(theme::float_sep(&p))
                                .child(theme::float_item(&p, "project-menu-finder", Some("folder-open"), "Open in Finder", None, false).on_click(act(Box::new(move |_, _, _| reveal(&f3)))))
                                .child(theme::float_item(&p, "project-menu-copy", Some("copy"), "Copy path", None, false).on_click(act(Box::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(f4.clone()))))))
                                .child(theme::float_item(&p, "project-menu-fold", Some(if folded { "eye" } else { "eye-off" }), if folded { "Show its sessions" } else { "Hide its sessions" }, None, false).on_click(act(Box::new(move |this, _, cx| this.toggle_project(&n, cx)))))
                                .when(empty, |d| {
                                    d.child(theme::float_sep(&p)).child(
                                        theme::float_item(&p, "project-menu-forget", Some("x"), "Remove from the sidebar", None, false)
                                            .on_click(act(Box::new(move |this, _, cx| this.forget_project(&f5, cx)))),
                                    )
                                })
                                .into_any_element()
                        }),
                    ),
                );
            row = row.child(div().relative().flex_shrink_0().h_full().flex().items_center().children(pill.map(|c| c.group_hover(group, |s| s.invisible()))).child(controls));
        } else {
            row = row.children(pill);
        }
        row.into_any_element()
    }

    /// a session's menu, from its row.
    pub(crate) fn session_menu(&self, s: &SessionSummary, group: SharedString, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let (sid, goal, folder) = (s.id, s.goal.clone(), s.cwd.clone());
        let rename = self.rename_input.clone();
        // clicks on the menu (opening it, closing it, choosing) are the menu's:
        // they don't reach the row underneath, which would open the session.
        div().id(SharedString::from(format!("session-{sid}-menu-zone"))).on_click(|_, _, cx| cx.stop_propagation()).invisible().group_hover(group, |st| st.visible()).child(
            Popover::new(SharedString::from(format!("session-{sid}-menu")))
                .p_0()
                .anchor(Anchor::TopRight)
                .on_open_change({
                    let rename = rename.clone();
                    let goal = goal.clone();
                    let me = me.clone();
                    move |open, window, cx| {
                        if *open {
                            rename.update(cx, |st, cx| st.set_value(goal.clone(), window, cx));
                            let _ = me.update(cx, |this, _| this.renaming = Some(sid));
                        }
                    }
                })
                .trigger(Button::new(SharedString::from(format!("session-{sid}-more"))).ghost().small().rounded(px(4.)).icon(Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
                .content(move |_, _, cx| {
                    let popover = cx.entity();
                    let Some(shell) = me.upgrade() else { return div().into_any_element() };
                    let this = shell.read(cx);
                    let p = this.palette.get();
                    let armed = this.confirm_delete == Some(sid);
                    let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>, close: bool| {
                        let (me, popover) = (me.clone(), popover.clone());
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            if close {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                            }
                            let _ = me.update(cx, |this, cx| f(this, window, cx));
                        }
                    };
                    let save = {
                        let (me, popover) = (me.clone(), popover.clone());
                        move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                            popover.update(cx, |st, cx| st.dismiss(window, cx));
                            let _ = me.update(cx, |this, cx| this.rename_session(sid, cx));
                        }
                    };
                    let f = folder.clone();
                    // renaming is the first row: a field shaped like an item, saved with Return.
                    let _ = save;
                    theme::float_list(&p)
                        .w(px(248.))
                        .child(
                            h_flex()
                                .min_h(theme::z(theme::FLOAT_ITEM_H))
                                .px(theme::z(10.))
                                .gap(theme::z(10.))
                                .rounded(theme::z(theme::R_FLOAT_ITEM))
                                .bg(p.fill)
                                .child(div().flex_shrink_0().size(theme::z(15.)).flex().items_center().justify_center().child(Icon::empty().path("icons/pencil.svg").size(theme::z(15.)).text_color(p.secondary)))
                                // the field's own inset off: the name starts where every item's label does.
                                .child(div().flex_1().text_size(theme::z(13.5)).child(Input::new(&rename).id(SharedString::from(format!("session-{sid}-name"))).appearance(false).px_0())),
                        )
                        .child(theme::float_sep(&p))
                        .child(theme::float_item(&p, "session-menu-finder", Some("folder-open"), "Open in Finder", None, false).on_click(act(Box::new(move |_, _, _| reveal(&f)), true)))
                        .child(theme::float_item(&p, "session-menu-copy", Some("copy"), "Copy session id", None, false).on_click(act(Box::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(sid.to_string()))), true)))
                        .child(theme::float_item(&p, "session-menu-workflow", Some("workflow"), "Make this a workflow", None, false).on_click(act(Box::new(move |this, _, cx| {
                            this.make_workflow_from(sid);
                            cx.notify();
                        }), true)))
                        .child(theme::float_sep(&p))
                        .child(
                            theme::float_danger(&p, SharedString::from(format!("session-{sid}-delete")), Some("trash"), if armed { "Click again to delete" } else { "Delete session" }, armed.then_some("It stays in Recently Deleted for 30 days"))
                                .on_click(act(Box::new(move |this, _, cx| this.delete(sid, cx)), armed)),
                        )
                        .into_any_element()
                }),
        )
        .into_any_element()
    }

    pub(crate) fn rename_session(&mut self, id: SessionId, cx: &mut Context<Self>) {
        let goal = self.rename_input.read(cx).value().trim().to_string();
        if !goal.is_empty() {
            self.cmd(Cmd::SetGoal { session: id, goal });
        }
        cx.notify();
    }
}

/// a project's colour, picked from its name: the same every time.
pub(crate) fn project_color(name: &str) -> Hsla {
    const HUES: [u32; 7] = [0x4f7fe3, 0x6c63e0, 0x9b62d6, 0x2a9d9a, 0x5f7488, 0x2f8fc9, 0xb35a9a];
    let sum = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b)));
    gpui_kit::rgb(HUES[(sum as usize) % HUES.len()]).into()
}

/// a project's tile: its first letter on two colours picked from its name,
/// the same every time (see `avatar`).
pub(crate) fn project_tile(name: &str, p: &Palette) -> Div {
    super::avatar::letter(name, 18., 5., p)
}

/// show a folder in the Finder.
fn reveal(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = path;

}

impl Shell {
    pub(crate) fn ask_deleted(&mut self) {
        self.cmd(Cmd::Ask { tag: "deleted.list", req: tessel_proto::Request::DeletedSessions });
    }

    pub(crate) fn restore_deleted(&mut self, session: SessionId) {
        self.cmd(Cmd::Ask { tag: "deleted.restore", req: tessel_proto::Request::SessionRestore { session } });
    }

    pub(crate) fn purge_deleted(&mut self, session: SessionId, cx: &mut Context<Self>) {
        if self.confirm_purge != Some(session) {
            self.confirm_purge = Some(session);
            cx.notify();
            return;
        }
        self.confirm_purge = None;
        self.deleted.retain(|d| d.id != session);
        self.cmd(Cmd::Ask { tag: "deleted.purge", req: tessel_proto::Request::SessionPurge { session } });
        cx.notify();
    }

    pub(crate) fn deleted_answer(&mut self, tag: &str, result: Result<tessel_proto::Response, String>, cx: &mut Context<Self>) {
        match (tag, result) {
            ("deleted.list", Ok(tessel_proto::Response::Deleted { sessions })) => self.deleted = sessions,
            ("deleted.list", _) => self.deleted.clear(),
            ("deleted.restore", Ok(tessel_proto::Response::Session { summary })) => {
                self.model.update_session(summary.clone());
                self.reflow_sidebar();
                self.ask_deleted();
                self.open(summary.id, cx);
            }
            ("deleted.restore", Err(e)) => self.error = Some(e),
            (_, Err(e)) => self.error = Some(e),
            _ => self.ask_deleted(),
        }
        cx.notify();
    }

    pub(crate) fn deleted_row(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.deleted.is_empty() {
            return None;
        }
        let p = self.palette.get();
        let me = cx.entity().downgrade();
        let n = self.deleted.len();
        let trigger = h_flex()
            .id("nav-deleted")
            .test_support()
            .h(px(30.))
            .mx_2()
            .px(px(8.))
            .gap(px(8.))
            .rounded(px(theme::R_TILE))
            .hover(|d| d.bg(p.hover))
            .child(Icon::empty().path("icons/trash.svg").size(theme::z(14.)).text_color(p.tertiary))
            .child(div().flex_1().set(Type::Callout).text_color(p.secondary).child("Recently Deleted"))
            .child(theme::count_pill(&p, n.to_string(), false));
        let popover = Popover::new("deleted-popover")
            .p_0()
            .anchor(Anchor::BottomLeft)
            .trigger(Button::new("nav-deleted-open").ghost().w_full().h_auto().p_0().child(trigger))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>, close: bool| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        if close {
                            popover.update(cx, |st, cx| st.dismiss(window, cx));
                        }
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                let mut list = v_flex().id("deleted-list").max_h(px(420.)).overflow_y_scroll().gap(px(2.));
                for d in &this.deleted {
                    let id = d.id;
                    let project = crate::model::folder_name(d.worktree.as_ref().map_or(d.cwd.as_str(), |w| w.repo.as_str()));
                    let mut about = vec![project, format!("deleted {}", deleted_ago(d.deleted_ms))];
                    if d.kept_work {
                        about.push("its changes kept".into());
                    }
                    let armed = this.confirm_purge == Some(id);
                    list = list.child(
                        h_flex()
                            .id(SharedString::from(format!("deleted-{id}")))
                            .test_support()
                            .px(px(8.))
                            .py(px(6.))
                            .gap(px(8.))
                            .rounded(px(theme::R_TILE))
                            .hover(|d| d.bg(p.hover))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().set(Type::Body).text_color(p.label).child(d.goal.clone()))
                                    .child(div().truncate().set(Type::Subheadline).text_color(p.tertiary).child(about.join(" · "))),
                            )
                            .child(
                                Button::new(SharedString::from(format!("deleted-{id}-restore")))
                                    .small()
                                    .outline()
                                    .label("Restore")
                                    .on_click(act(Box::new(move |this, _, _| this.restore_deleted(id)), true)),
                            )
                            .child(
                                Button::new(SharedString::from(format!("deleted-{id}-purge")))
                                    .small()
                                    .ghost()
                                    .when(armed, |b| b.danger())
                                    .label(if armed { "Delete forever?" } else { "Delete" })
                                    .tooltip("Delete it for good")
                                    .on_click(act(Box::new(move |this, _, cx| this.purge_deleted(id, cx)), false)),
                            ),
                    );
                }
                v_flex()
                    .w(px(380.))
                    .p(px(6.))
                    .gap(px(4.))
                    .child(div().px(px(8.)).pt(px(4.)).set(Type::Subheadline).text_color(p.tertiary).child("Kept for 30 days, then deleted for good."))
                    .child(list)
                    .into_any_element()
            });
        Some(div().pt(px(8.)).child(popover).into_any_element())
    }
}

fn deleted_ago(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", s / 60),
        3600..86_400 => format!("{}h ago", s / 3600),
        86_400..172_800 => "yesterday".into(),
        _ => format!("{} days ago", s / 86_400),
    }
}
