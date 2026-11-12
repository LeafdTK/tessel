//! shipping a workspace's work, from the conversation's header: a pull
//! request (opened, updated, squashed or rebased in), or straight into main
//! for those who don't use them. what happened is said in a line above the
//! chat, with the pull request's link; a conflict comes with a button that
//! hands it to the agent.

use super::Shell;
use crate::bridge::Cmd;
use crate::theme::{self, Type, Typeset as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, Sizable as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tessel_proto::{Request, ShipStep};

impl Shell {
    /// take a step with the open chat's work.
    pub(crate) fn ship(&mut self, step: ShipStep, cx: &mut Context<Self>) {
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else { return };
        self.shipping = true;
        self.shipped = None;
        self.cmd(Cmd::Ask { tag: "ship", req: Request::Ship { session, step } });
        cx.notify();
    }

    pub(crate) fn ship_answer(&mut self, result: Result<tessel_proto::Response, String>) {
        self.shipping = false;
        self.shipped = Some(match result {
            Ok(tessel_proto::Response::Shipped { said, url }) => Ok((said, url)),
            Ok(other) => Err(format!("unexpected answer: {other:?}")),
            Err(e) => Err(e),
        });
        // the branch and its pull request have moved.
        if let Some(session) = self.model.open.as_ref().map(|o| o.id) {
            self.ws.pr_loading = true;
            self.cmd(Cmd::Pr(session));
        }
        self.workspace_touched();
    }

    /// a conflict, for the agent to sort out: what to do, in so many words.
    fn resolve_with_agent(&mut self, said: &str, cx: &mut Context<Self>) {
        let Some(session) = self.model.open.as_ref().map(|o| o.id) else { return };
        let text = format!(
            "Bring this branch up to date with the main branch on origin. Updating it stopped: {said} Fetch origin, rebase onto its main branch, resolve the conflicts keeping what both sides meant, run the tests, and finish the rebase."
        );
        self.cmd(Cmd::Send { session, text, attachments: Vec::new() });
        self.shipped = None;
        cx.notify();
    }

    /// what the last step did, above the chat.
    pub(crate) fn shipped_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        if self.shipping {
            return Some(h_flex().id("shipped").test_support().px_4().py_2().gap(px(8.)).items_center().set(Type::Callout).text_color(p.secondary).child(super::live_dot(p.blocked, &p)).child("Working on it…").into_any_element());
        }
        let r = self.shipped.as_ref()?;
        let (text, ink) = match r {
            Ok((said, _)) => (said.clone(), p.label),
            Err(e) => (e.clone(), p.failed),
        };
        let conflict = matches!(r, Err(e) if e.starts_with("Conflicts in"));
        let url = r.as_ref().ok().and_then(|(_, u)| u.clone());
        Some(
            h_flex()
                .id("shipped")
                .test_support()
                .px_4()
                .py_2()
                .gap(px(8.))
                .items_center()
                .child(div().flex_1().min_w_0().set(Type::Callout).text_color(ink).child(text.clone()))
                .when_some(url, |d, url| d.child(Button::new("shipped-open").ghost().small().label("Open").icon(Icon::empty().path("icons/arrow-up-right.svg")).on_click(move |_, _, cx| cx.open_url(&url))))
                .when(conflict, |d| {
                    d.child(Button::new("shipped-agent").outline().small().label("Have the agent resolve it").on_click(cx.listener(move |this, _, _, cx| this.resolve_with_agent(&text, cx))))
                })
                .child(Button::new("shipped-dismiss").ghost().small().icon(Icon::empty().path("icons/x.svg")).on_click(cx.listener(|this, _, _, cx| {
                    this.shipped = None;
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    /// the Ship menu, in the conversation's header.
    pub(crate) fn ship_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let summary = self.model.open_summary()?;
        if self.show_new {
            return None;
        }
        let session = summary.id;
        let me = cx.entity().downgrade();
        let menu = Popover::new("ship-menu")
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(Button::new("ship").ghost().small().icon(Icon::empty().path("icons/git-pull-request-arrow.svg")).tooltip("Ship this work"))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let step = |s: ShipStep| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| this.ship(s.clone(), cx));
                    }
                };
                // what there is to do depends on whether there's a pull request.
                let pr = this.ws.pr.as_ref().filter(|_| this.ws_session == Some(session)).and_then(|s| s.pr.clone());
                let open_pr = pr.as_ref().filter(|p| p.state.eq_ignore_ascii_case("open"));
                let goal = this.model.open_summary().map(|s| s.goal.clone()).unwrap_or_default();
                let mut list = theme::float_list(&p).w(px(300.)).child(theme::float_label(&p, "Pull request"));
                match open_pr {
                    Some(pr) => {
                        list = list
                            .child(div().px(px(10.)).pb(px(4.)).set(Type::Subheadline).text_color(p.secondary).child(format!("#{} {}", pr.number, pr.title)))
                            .child(theme::float_item(&p, "ship-squash", Some("git-merge"), "Squash and merge", Some("One commit on main"), false).test_support().on_click(step(ShipStep::MergePullRequest { squash: true })))
                            .child(theme::float_item(&p, "ship-rebase", Some("git-branch"), "Rebase and merge", Some("Every commit, on top of main"), false).test_support().on_click(step(ShipStep::MergePullRequest { squash: false })));
                    }
                    None => {
                        list = list
                            .child(theme::float_item(&p, "ship-pr", Some("git-pull-request"), "Create pull request", Some("Push, then open one into main"), false).test_support().on_click(step(ShipStep::PullRequest { title: goal.clone(), body: String::new(), draft: false })))
                            .child(theme::float_item(&p, "ship-draft", Some("git-pull-request-draft"), "Create draft pull request", None, false).test_support().on_click(step(ShipStep::PullRequest { title: goal.clone(), body: String::new(), draft: true })));
                    }
                }
                list = list
                    .child(theme::float_item(&p, "ship-update", Some("refresh-cw"), "Update from main", Some("Replay this branch on main as it is now"), false).test_support().on_click(step(ShipStep::Update)))
                    .child(theme::float_item(&p, "ship-push", Some("upload"), "Push", None, false).test_support().on_click(step(ShipStep::Push)))
                    .child(theme::float_sep(&p))
                    .child(theme::float_label(&p, "Straight into main"))
                    .child(theme::float_item(&p, "ship-main", Some("git-commit-horizontal"), "Squash into main", Some("One commit, on main here"), false).test_support().on_click(step(ShipStep::MergeIntoMain { squash: true, message: goal.clone(), push: false })))
                    .child(theme::float_item(&p, "ship-main-push", Some("upload"), "Squash into main and push", None, false).test_support().on_click(step(ShipStep::MergeIntoMain { squash: true, message: goal.clone(), push: true })))
                    .child(theme::float_item(&p, "ship-main-rebase", Some("git-branch"), "Rebase into main", Some("Every commit, on top of main"), false).test_support().on_click(step(ShipStep::MergeIntoMain { squash: false, message: String::new(), push: false })));
                list.into_any_element()
            });
        // opening the menu asks after the pull request, so it offers the right things.
        Some(
            div()
                .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, _| {
                    if !this.ws.pr_loading {
                        this.ws.pr_loading = true;
                        this.cmd(Cmd::Pr(session));
                    }
                }))
                .child(menu)
                .into_any_element(),
        )
    }
}
