//! visual mode: watching the agent work. with it on, the agent's own Bash
//! commands show in a terminal as they run (each headed by what it's for),
//! and in the browser you see it move, outline what it's about to use, and
//! type a letter at a time.
//!
//! it's an attribute of a chat, not a kind of chat: the eye in the chat's
//! header turns it on and off.

use super::Shell;
use crate::bridge::Cmd;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, Sizable as _};
use gpui_kit::*;

impl Shell {
    /// visual mode on or off for the open chat, and the panes to watch it in.
    pub(crate) fn set_visual(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(session) = self.model.open_summary().map(|s| s.id) else { return };
        self.cmd(Cmd::SetVisual { session, visual: on });
        if on {
            self.show_visual_panes(window, cx);
        }
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

    /// the eye in the chat's header: visual mode, on and off.
    pub(crate) fn visual_button(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let summary = self.model.open_summary()?;
        if self.show_new {
            return None;
        }
        let on = summary.visual;
        let p = self.palette.get();
        Some(
            Button::new("visual")
                .ghost()
                .small()
                .icon(Icon::empty().path(if on { "icons/eye.svg" } else { "icons/eye-off.svg" }).text_color(if on { p.accent } else { p.secondary }))
                .tooltip(if on { "Stop watching the agent work" } else { "Watch the agent work: its commands live in a terminal, its browser steps paced" })
                .on_click(cx.listener(move |this, _, window, cx| this.set_visual(!on, window, cx)))
                .into_any_element(),
        )
    }
}
