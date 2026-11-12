//! visual mode: watching the agent work. with it on, the agent runs its
//! commands in terminals you can see (the terminal pane follows the latest),
//! and in the browser you see it move, outline what it's about to use, and
//! type a letter at a time. its own Bash is off unless you allow it.
//!
//! it's an attribute of a chat, not a kind of chat: the eye in the chat's
//! header turns it on and off.

use super::Shell;
use crate::bridge::Cmd;
use crate::theme;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::*;

impl Shell {
    /// visual mode on or off for the open chat, and the panes to watch it in.
    pub(crate) fn set_visual(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.model.open_summary().map(|s| s.id) else { return };
        self.cmd(Cmd::SetVisual { session, visual: Some(on), bash: None });
        if on {
            self.show_visual_panes(window, cx);
        }
        cx.notify();
    }

    pub(crate) fn set_visual_bash(&mut self, on: bool, cx: &mut Context<Self>) {
        let Some(session) = self.model.open_summary().map(|s| s.id) else { return };
        self.cmd(Cmd::SetVisual { session, visual: None, bash: Some(on) });
        cx.notify();
    }

    /// a terminal and the browser, beside the chat, unless they're there already.
    pub(crate) fn show_visual_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has = |this: &Shell, m: crate::ui::Module| this.ui.panes().iter().any(|p| p.module == Some(m) && this.pane_open(p));
        if !has(self, crate::ui::Module::Browser) {
            self.show_browser(window, cx);
        }
        if !has(self, crate::ui::Module::Terminal) {
            self.new_terminal(window, cx);
        }
    }

    /// the eye in the chat's header: visual mode, and whether Bash stays.
    pub(crate) fn visual_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let summary = self.model.open_summary()?;
        if self.show_new {
            return None;
        }
        let (on, bash) = (summary.visual, summary.visual_bash);
        let me = cx.entity().downgrade();
        let p = self.palette.get();
        Some(
            Popover::new("visual-menu")
                .p_0()
                .anchor(Anchor::TopRight)
                .trigger(
                    Button::new("visual")
                        .ghost()
                        .small()
                        .icon(Icon::empty().path(if on { "icons/eye.svg" } else { "icons/eye-off.svg" }).text_color(if on { p.accent } else { p.secondary }))
                        .tooltip(if on { "Visual mode is on" } else { "Visual mode" }),
                )
                .content(move |_, _, cx| {
                    let popover = cx.entity();
                    let Some(shell) = me.upgrade() else { return div().into_any_element() };
                    let p = shell.read(cx).palette.get();
                    let (me1, me2, pop1, pop2) = (me.clone(), me.clone(), popover.clone(), popover.clone());
                    theme::float_list(&p)
                        .w(px(300.))
                        .child(
                            theme::float_item(&p, "visual-toggle", Some("eye"), if on { "Stop watching" } else { "Watch the agent work" }, Some("Its commands in a terminal, its browser steps visible"), on)
                                .test_support()
                                .on_click(move |_, window, cx| {
                                    pop1.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me1.update(cx, |this, cx| this.set_visual(!on, window, cx));
                                }),
                        )
                        .child(
                            theme::float_item(&p, "visual-bash", Some("square-terminal"), "Let it use Bash too", Some("Unseen, for quick checks. Off by default"), bash)
                                .test_support()
                                .on_click(move |_, window, cx| {
                                    pop2.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me2.update(cx, |this, cx| this.set_visual_bash(!bash, cx));
                                }),
                        )
                        .into_any_element()
                })
                .into_any_element(),
        )
    }
}
