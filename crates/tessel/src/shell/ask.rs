//! asking an agent to change the window.
//!
//! a bar floats over everything: say what you want and an agent, started
//! beside your work rather than in place of it, edits `ui.json`. the window
//! reloads on each save and the panes move to where it put them, so you watch
//! the change happen. later asks go to the same agent, which remembers.

use super::*;
use gpui_kit::component::Icon;

/// what the agent is told the first time, before what you asked.
const BRIEF: &str = "You are changing tessel's window while the person watches it. The reference is ui/README.md in this folder: read it first. \
Edit ui.json in this folder; tessel reloads it on every save and animates the change, so make one coherent edit at a time. \
After each save read ui/check.txt and fix what it reports. Change only what is asked, keep the theme unless asked, and answer in one short sentence when done.\n\nWhat they want: ";

impl Shell {
    pub fn open_ask(&mut self, cx: &mut Context<Self>) {
        self.ask_open = true;
        self.focus_ask = true;
        cx.notify();
    }

    pub fn close_ask(&mut self, cx: &mut Context<Self>) {
        self.ask_open = false;
        cx.notify();
    }

    /// send what is typed to the layout agent, starting it if there isn't one.
    pub(crate) fn ask(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.ask_input.read(cx).value().trim().to_string();
        if text.is_empty() {
            return;
        }
        let Some(home) = self.ui_path.as_deref().and_then(std::path::Path::parent).map(|p| p.to_string_lossy().into_owned()) else {
            self.error = Some("This window wasn't started with a ui.json to change.".into());
            cx.notify();
            return;
        };
        let alive = self.layout_agent.filter(|id| self.model.summary(*id).is_some());
        match alive {
            Some(session) => self.cmd(Cmd::Send { session, text, attachments: vec![] }),
            None => {
                self.layout_agent = None;
                self.cmd(Cmd::CreateAside { cwd: home, goal: "Changing this window".into(), prompt: format!("{BRIEF}{text}"), mode: "acceptEdits".into() });
            }
        }
        self.ask_sent = true;
        self.ask_input.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub(crate) fn ask_layer(&self, now: Millis, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let agent = self.layout_agent.and_then(|id| self.model.summary(id));
        // what the agent is doing, in a few words.
        let status: Option<(String, Hsla)> = match agent {
            Some(s) if s.run_state.is_busy() => Some((
                match (&s.activity, s.activity_since_ms) {
                    (Some(a), Some(since)) => format!("Rearranging · {a} · {}", ago(since, now)),
                    _ => "Rearranging…".into(),
                },
                p.secondary,
            )),
            Some(s) if matches!(s.run_state, RunState::NeedsPermission | RunState::NeedsInput) => Some(("It's waiting for you in its session.".into(), p.blocked)),
            Some(_) if self.ask_sent => Some(("Done. Ask for something else, or press Escape.".into(), p.secondary)),
            None if self.ask_sent => Some(("Starting…".into(), p.secondary)),
            _ => None,
        };
        let busy = agent.is_some_and(|s| s.run_state.is_busy()) || (self.ask_sent && agent.is_none());
        div()
            .id("ask-layer")
            .absolute()
            .left_0()
            .right_0()
            .bottom(px(28.))
            .flex()
            .justify_center()
            .track_focus(&self.ask_focus)
            .key_context("Ask")
            .on_action(cx.listener(|this, _: &crate::CloseAsk, _, cx| this.close_ask(cx)))
            .child(
                v_flex()
                    .id("ask-bar")
                    .test_support()
                    .aria_label("Ask an agent to change this window")
                    .w(px(560.))
                    .max_w(relative(0.9))
                    .p(px(theme::FLOAT_PAD))
                    .gap_1()
                    .rounded(px(theme::R_PANEL))
                    .border_1()
                    .border_color(if busy { p.accent.opacity(0.5) } else { p.separator })
                    .bg(p.raised)
                    .shadow_lg()
                    .child(
                        h_flex()
                            .h(px(theme::FLOAT_ITEM_H))
                            .px(px(10.))
                            .gap(px(10.))
                            .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                            .child(div().text_color(p.accent).child(Icon::empty().path(if busy { "icons/loader.svg" } else { "icons/sparkles.svg" }).small()))
                            .child(div().flex_1().child(Input::new(&self.ask_input).id("ask-input").appearance(false)))
                            .child(Button::new("ask-close").ghost().xsmall().icon(Icon::empty().path("icons/x.svg")).tooltip("Close").on_click(cx.listener(|this, _, _, cx| this.close_ask(cx)))),
                    )
                    .children(status.map(|(text, color)| div().px(px(10.)).pb(px(6.)).set(Type::Subheadline).text_color(color).child(text))),
            )
    }
}
