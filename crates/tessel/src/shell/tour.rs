//! the tour: what Tessel can do, in nine short cards, once, the first time it
//! runs (and again whenever asked: `app.tour`, or Help in the menu bar).
//!
//! each card sits beside the part of the window it's about, and where trying
//! a thing says more than reading about it, a button does it.

use super::Shell;
use crate::theme::{self, Type, Typeset as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

/// where a card sits.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Spot {
    Middle,
    /// beside the sidebar.
    Left,
    /// by the sidebar's foot.
    LowerLeft,
    /// under the conversation's header.
    UpperRight,
    /// over the message box.
    Bottom,
}

struct Step {
    title: &'static str,
    body: &'static str,
    spot: Spot,
    /// a command to try it with, and what the button says.
    try_it: Option<(&'static str, &'static str)>,
}

const STEPS: [Step; 9] = [
    Step {
        title: "Welcome to Tessel",
        body: "Run several coding agents at once. Each chat can work in its own copy of the repository (a git worktree), so agents never get in each other's way, or in yours.",
        spot: Spot::Middle,
        try_it: None,
    },
    Step {
        title: "Start a chat",
        body: "⌘N, or + at the top of the sidebar. Pick a project, say what you want done, and choose whether it works in place or in a new worktree.",
        spot: Spot::Left,
        try_it: None,
    },
    Step {
        title: "Agents waiting on you come first",
        body: "Chats waiting on an answer or a permission collect at the top of the sidebar, and ⌘J goes to the next one. On a MacBook with a notch, they show around the camera too, whatever app you're in.",
        spot: Spot::Left,
        try_it: None,
    },
    Step {
        title: "Desktops",
        body: "Keep projects apart: each desktop has its own panes and chats, and lists only the projects you pick. Switch with ⌘1 to ⌘9, or swipe sideways on the sidebar with two fingers.",
        spot: Spot::LowerLeft,
        try_it: Some(("desktop.new", "Make a desktop")),
    },
    Step {
        title: "Panes",
        body: "Split any pane with ⌘D and choose what it shows: a terminal, the git graph, the changes, another chat. Two conversation panes can show two chats side by side, or the same chat twice.",
        spot: Spot::UpperRight,
        try_it: None,
    },
    Step {
        title: "Run it, then ship it",
        body: "The play button runs this workspace's dev server on ports of its own; set its scripts up once, by hand or with an agent. The branch button opens a pull request, or merges straight into main.",
        spot: Spot::UpperRight,
        try_it: None,
    },
    Step {
        title: "A browser you share",
        body: "The agent can open your app in Tessel's browser and use it while you watch. Press Pick and click part of the page to show the agent exactly what you mean.",
        spot: Spot::UpperRight,
        try_it: Some(("browser.open", "Open the browser")),
    },
    Step {
        title: "Take a turn back",
        body: "The … under each of the agent's turns forks the chat into a new tab or workspace, or puts the files back the way they were before that turn.",
        spot: Spot::Bottom,
        try_it: None,
    },
    Step {
        title: "⌘K does everything",
        body: "The palette finds commands, chats, files and themes. Everything in Tessel is a command, so scripts (tesselctl ui) and agents can do anything you can.",
        spot: Spot::Middle,
        try_it: Some(("app.palette", "Open the palette")),
    },
];

impl Shell {
    pub(crate) fn start_tour(&mut self, cx: &mut Context<Self>) {
        self.tour = Some(0);
        cx.notify();
    }

    fn end_tour(&mut self, cx: &mut Context<Self>) {
        self.tour = None;
        if let Some(seen) = &self.tour_seen {
            let _ = std::fs::write(seen, "");
        }
        cx.notify();
    }

    /// the card for the step showing, over a dimmed window.
    pub(crate) fn tour_layer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let at = self.tour?;
        let step = STEPS.get(at)?;
        let p = self.palette.get();
        let last = at + 1 == STEPS.len();
        let card = v_flex()
            .id("tour-card")
            .test_support()
            .w(px(360.))
            .p(px(18.))
            .gap(px(10.))
            .rounded(px(theme::R_CARD + 4.))
            .border_1()
            .border_color(p.separator)
            .bg(p.raised)
            .shadow_lg()
            .child(div().set(Type::Subheadline).text_color(p.tertiary).child(format!("{} of {}", at + 1, STEPS.len())))
            .child(div().set(Type::Title).text_color(p.label).child(step.title))
            .child(div().set(Type::Callout).text_color(p.secondary).child(step.body))
            .child(
                h_flex()
                    .pt(px(6.))
                    .gap(px(6.))
                    .items_center()
                    .child(Button::new("tour-skip").ghost().small().label(if last { "Close" } else { "Skip" }).on_click(cx.listener(|this, _, _, cx| this.end_tour(cx))))
                    .child(div().flex_1())
                    .when_some(step.try_it, |d, (cmd, label)| {
                        d.child(Button::new("tour-try").outline().small().label(label).on_click(cx.listener(move |this, _, window, cx| {
                            // trying it is the tour's last word on this step.
                            let next = this.tour.map(|t| t + 1).filter(|t| *t < STEPS.len());
                            this.tour = None;
                            this.run_key(cmd, serde_json::json!({}), window, cx);
                            this.tour = next;
                            if next.is_none() {
                                this.end_tour(cx);
                            }
                        })))
                    })
                    .when(at > 0, |d| {
                        d.child(Button::new("tour-back").ghost().small().label("Back").on_click(cx.listener(|this, _, _, cx| {
                            this.tour = this.tour.map(|t| t.saturating_sub(1));
                            cx.notify();
                        })))
                    })
                    .child(Button::new("tour-next").primary().small().label(if last { "Done" } else { "Next" }).on_click(cx.listener(move |this, _, _, cx| {
                        if last {
                            this.end_tour(cx);
                        } else {
                            this.tour = this.tour.map(|t| t + 1);
                            cx.notify();
                        }
                    }))),
            );
        let placed = match step.spot {
            Spot::Middle => div().size_full().flex().items_center().justify_center().child(card),
            Spot::Left => div().size_full().flex().items_start().pt(px(120.)).pl(px(300.)).child(card),
            Spot::LowerLeft => div().size_full().flex().items_end().pb(px(56.)).pl(px(24.)).child(card),
            Spot::UpperRight => div().size_full().flex().justify_end().items_start().pt(px(64.)).pr(px(24.)).child(card),
            Spot::Bottom => div().size_full().flex().justify_center().items_end().pb(px(150.)).child(card),
        };
        Some(
            div()
                .id("tour")
                .absolute()
                .inset_0()
                .occlude()
                .bg(gpui_kit::black().opacity(if p.dark { 0.35 } else { 0.18 }))
                .child(placed)
                .into_any_element(),
        )
    }
}
