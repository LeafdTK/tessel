//! what the sidebar lets you do with a project or a session: start another
//! session there, open its folder, rename it, fold a project away, delete.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;

impl Shell {
    /// the folder a project's sessions run in: its repository for worktrees.
    pub(crate) fn project_folder(&self, project: &str) -> Option<String> {
        self.model.sessions.iter().find(|s| project_name(s) == project).map(|s| s.worktree.as_ref().map_or(s.cwd.clone(), |w| w.repo.clone()))
    }

    pub(crate) fn toggle_project(&mut self, project: &str, cx: &mut Context<Self>) {
        if !self.model.collapsed.remove(project) {
            self.model.collapsed.insert(project.to_owned());
        }
        // a fold is yours: it happens now, even with the pointer over the list.
        self.reflow_sidebar_as(false);
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
            div().size(theme::z(18.)).flex_shrink_0().flex().items_center().justify_center().rounded(theme::z(5.)).bg(p.blocked.opacity(0.16)).child(Icon::empty().path("icons/bell.svg").size(theme::z(11.)).text_color(p.blocked))
        } else {
            project_tile(title, &p)
        };
        let mut row = h_flex()
            .id(SharedString::from(format!("project-{title}")))
            .test_support()
            .group(group.clone())
            // a little air above and below, so a hovered header never touches a selected row.
            .h(px(crate::model::HEADER_H - 3.))
            .my(px(1.5))
            .mx_2()
            .pl(px(4.))
            .pr(px(8.))
            .gap(px(6.))
            .items_center()
            .rounded(px(theme::R_TILE))
            .when(!queue, |d| {
                let n = name.clone();
                d.hover(|d| d.bg(p.hover)).on_click(cx.listener(move |this, _, _, cx| this.toggle_project(&n, cx)))
            })
            // the disclosure: which way it points says whether it is open.
            .child(div().w(px(12.)).flex_shrink_0().flex().justify_center().when(!queue, |d| d.child(Icon::empty().path(if folded { "icons/chevron-right.svg" } else { "icons/chevron-down.svg" }).size(theme::z(12.)).text_color(p.tertiary))))
            .child(tile)
            .child(div().flex_1().min_w_0().truncate().set(Type::Body).font_weight(FontWeight::MEDIUM).text_color(if queue { p.label } else { p.label }).child(title.to_owned()))
            ;
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
                .child(Button::new(SharedString::from(format!("project-{title}-new"))).ghost().small().icon(Icon::empty().path("icons/plus.svg")).tooltip("New Session Here").on_click(cx.listener(move |this, _, window, cx| {
                    cx.stop_propagation();
                    this.new_worktree = false;
                    this.start_in(f1.clone(), window, cx);
                })))
                .child(
                    // the menu's clicks stay the menu's: the row underneath folds the project on a click.
                    div().id(SharedString::from(format!("project-{title}-menu-zone"))).on_click(|_, _, cx| cx.stop_propagation()).child(
                    Popover::new(SharedString::from(format!("project-{title}-menu")))
                        .p_0()
                        .anchor(Anchor::TopRight)
                        .trigger(Button::new(SharedString::from(format!("project-{title}-more"))).ghost().small().icon(Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
                        .content(move |_, _, cx| {
                            let popover = cx.entity();
                            let Some(shell) = me.upgrade() else { return div().into_any_element() };
                            let p = shell.read(cx).palette.get();
                            let folded = shell.read(cx).model.collapsed.contains(&name);
                            let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>| {
                                let (me, popover) = (me.clone(), popover.clone());
                                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                    popover.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me.update(cx, |this, cx| f(this, window, cx));
                                }
                            };
                            let (f1, f2, f3, f4, n) = (folder.clone(), folder.clone(), folder.clone(), folder.clone(), name.clone());
                            theme::float_list(&p)
                                .w(px(240.))
                                .child(theme::float_item(&p, "project-menu-new", Some("plus"), "New session", None, false).on_click(act(Box::new(move |this, w, cx| {
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
                .trigger(Button::new(SharedString::from(format!("session-{sid}-more"))).ghost().small().icon(Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
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
                                .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                                .bg(p.fill)
                                .child(div().flex_shrink_0().size(theme::z(15.)).flex().items_center().justify_center().child(Icon::empty().path("icons/pencil.svg").size(theme::z(15.)).text_color(p.secondary)))
                                .child(div().flex_1().text_size(theme::z(13.5)).child(Input::new(&rename).id(SharedString::from(format!("session-{sid}-name"))).appearance(false))),
                        )
                        .child(theme::float_sep(&p))
                        .child(theme::float_item(&p, "session-menu-finder", Some("folder-open"), "Open in Finder", None, false).on_click(act(Box::new(move |_, _, _| reveal(&f)), true)))
                        .child(theme::float_item(&p, "session-menu-copy", Some("copy"), "Copy session id", None, false).on_click(act(Box::new(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(sid.to_string()))), true)))
                        .child(theme::float_sep(&p))
                        .child(
                            theme::float_danger(&p, SharedString::from(format!("session-{sid}-delete")), Some("trash"), if armed { "Click again to delete" } else { "Delete session" }, armed.then_some("Its history can't be brought back"))
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

/// a project's tile: its first letter on a colour of its own, picked from its
/// name so it is the same every time. identity colours, never the ones that
/// mean something (orange, red, green).
pub(crate) fn project_tile(name: &str, p: &Palette) -> Div {
    let hue = project_color(name);
    let letter = name.chars().find(|c| c.is_alphanumeric()).map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "·".into());
    let _ = p;
    div()
        .size(theme::z(18.))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::z(5.))
        .bg(hue)
        .text_size(theme::z(10.5))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(gpui_kit::white())
        .child(letter)
}

/// show a folder in the Finder.
fn reveal(path: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(path).spawn();
    #[cfg(not(target_os = "macos"))]
    let _ = path;
}
