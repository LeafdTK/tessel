//! several chats on screen at once.
//!
//! every conversation pane holds a chat. the one you type into (`chat_pane`)
//! shows `model.open`; each other one is a `ChatView`: the chat it holds and
//! its own scroll. two panes holding the same chat are mirrors: one transcript,
//! one draft, two places to read it from. splitting a conversation pane makes
//! a mirror; opening a chat changes only the focused pane; clicking a pane
//! moves typing there, chat and all.

use super::Shell;
use crate::bridge::Cmd;
use crate::model::Change;
use crate::theme::{self, Type, Typeset as _};
use crate::ui::Module;
use gpui_kit::component::message_scroller::MessageScrollerState;
use gpui_kit::component::{h_flex, v_flex};
use gpui_kit::*;
use tessel_proto::{RunState, SessionId};

/// a conversation pane that isn't the one you type into.
pub(crate) struct ChatView {
    pub(crate) session: SessionId,
    pub(crate) scroller: Entity<MessageScrollerState>,
}

impl Shell {
    /// bring the panes' chats in line with the layout: a conversation pane
    /// that has just appeared shows the focused chat, one that has gone takes
    /// its chat with it, and the daemon is told what to follow.
    pub(crate) fn sync_views(&mut self, cx: &mut Context<Self>) {
        let panes: Vec<String> = self.ui.panes().iter().filter(|p| p.module == Some(Module::Conversation)).map(|p| p.id.clone()).collect();
        if !self.chat_pane.as_ref().is_some_and(|c| panes.contains(c)) {
            let next = self.pane_focus.clone().filter(|f| panes.contains(f)).or_else(|| panes.first().cloned());
            self.chat_pane = None;
            if let Some(next) = next {
                self.focus_chat_pane(&next, cx);
            }
        }
        let focus = self.chat_pane.clone();
        self.views.retain(|id, _| panes.contains(id) && Some(id) != focus.as_ref());
        for id in &panes {
            if Some(id) == focus.as_ref() || self.views.contains_key(id) {
                continue;
            }
            let Some(open) = self.model.open.as_ref() else { continue };
            let (session, n) = (open.id, open.len());
            let scroller = cx.new(|cx| MessageScrollerState::new(0, cx));
            scroller.update(cx, |s, cx| {
                s.reset(n, cx);
                s.scroll_to_end(cx);
            });
            self.views.insert(id.clone(), ChatView { session, scroller });
        }
        self.sync_follow();
    }

    /// tell the daemon which chats besides the focused one are on screen,
    /// and let go of any no pane shows.
    pub(crate) fn sync_follow(&mut self) {
        let open = self.model.open.as_ref().map(|o| o.id).or(self.awaiting);
        let mut extra: Vec<SessionId> = Vec::new();
        // what other panes show, and what other desktops do: switching back is instant.
        for s in self.views.values().map(|v| v.session).chain(self.stashed_sessions()) {
            if Some(s) != open && !extra.contains(&s) {
                extra.push(s);
            }
        }
        extra.sort();
        self.model.others.retain(|id, _| extra.contains(id));
        if extra != self.followed {
            self.followed = extra.clone();
            self.cmd(Cmd::Follow(extra));
        }
    }

    /// is a chat on screen in a pane other than the focused one.
    pub(crate) fn in_a_view(&self, session: SessionId) -> bool {
        self.views.values().any(|v| v.session == session)
    }

    /// make `session` the focused chat. the one it replaces stays loaded if
    /// another pane still shows it. `keep_scroll`: the focused pane's scroll
    /// already belongs to the new chat (it came from that chat's pane).
    pub(crate) fn switch_open(&mut self, session: SessionId, keep_scroll: bool, cx: &mut Context<Self>) {
        if self.model.open.as_ref().is_some_and(|o| o.id == session) {
            return;
        }
        // the draft stays with its chat.
        let text = self.composer.read(cx).value().to_string();
        if let Some(old) = self.model.open.as_ref().map(|o| o.id).or(self.awaiting) {
            if text.trim().is_empty() {
                self.drafts.remove(&old);
            } else {
                self.drafts.insert(old, text);
            }
        }
        self.draft_to_set = Some(self.drafts.remove(&session).unwrap_or_default());
        if let Some(old) = self.model.open.take()
            && self.in_a_view(old.id)
        {
            self.model.others.insert(old.id, old);
        }
        self.answers.clear();
        self.prompt_open.snap(0.0);
        self.prompt_ghost.clear();
        match self.model.others.remove(&session) {
            // on screen already: no need to ask the daemon for it again.
            Some(open) => {
                let n = open.len();
                self.model.open = Some(open);
                self.awaiting = None;
                if !keep_scroll {
                    self.scroller.update(cx, |s, cx| {
                        s.reset(n, cx);
                        s.scroll_to_end(cx);
                    });
                }
                self.sync_prompt(false);
                self.mark_open_seen();
            }
            None => {
                self.awaiting = Some(session);
                if !keep_scroll {
                    self.scroller.update(cx, |s, cx| s.reset(0, cx));
                }
            }
        }
        self.sync_follow();
        self.cmd(Cmd::Open(Some(session)));
    }

    /// move typing to another conversation pane. it brings its chat and its
    /// scroll; the pane it leaves keeps showing its own.
    pub(crate) fn focus_chat_pane(&mut self, pane: &str, cx: &mut Context<Self>) {
        if self.chat_pane.as_deref() == Some(pane) {
            return;
        }
        let Some(view) = self.views.remove(pane) else {
            self.chat_pane = Some(pane.to_owned());
            return;
        };
        if let Some(left) = self.chat_pane.take()
            && let Some(session) = self.model.open.as_ref().map(|o| o.id).or(self.awaiting)
        {
            self.views.insert(left, ChatView { session, scroller: self.scroller.clone() });
        }
        self.scroller = view.scroller;
        self.chat_pane = Some(pane.to_owned());
        self.show_new = false;
        self.switch_open(view.session, true, cx);
        cx.notify();
    }

    /// what arrived for a chat changes every pane showing it.
    pub(crate) fn relayout_in(&mut self, session: SessionId, change: Change, len: usize, cx: &mut Context<Self>) {
        if self.model.open.as_ref().is_some_and(|o| o.id == session) {
            self.relayout(change, len, cx);
        }
        let scrollers: Vec<_> = self.views.values().filter(|v| v.session == session).map(|v| v.scroller.clone()).collect();
        for s in scrollers {
            s.update(cx, |s, cx| match change {
                Change::None => {}
                Change::Appended(n) => {
                    s.append(n, cx);
                }
                Change::Row(i) => {
                    s.remeasure_items(i..i + 1, cx);
                }
                Change::Reset => {
                    s.reset(len, cx);
                }
            });
        }
    }

    /// a fresh copy of a chat arrived: every pane showing it starts over at its end.
    pub(crate) fn reset_views_of(&mut self, session: SessionId, len: usize, cx: &mut Context<Self>) {
        let scrollers: Vec<_> = self.views.values().filter(|v| v.session == session).map(|v| v.scroller.clone()).collect();
        for s in scrollers {
            s.update(cx, |s, cx| {
                s.reset(len, cx);
                s.scroll_to_end(cx);
            });
        }
    }

    /// a conversation pane you aren't typing into: its chat, live, and where
    /// the message box would be, a bar that brings typing here.
    pub(crate) fn chat_view_el(&self, pane: &str, view: &ChatView, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let t = self.transcript_opts;
        let column = || div().w_full().max_w(px(t.width)).px(px(t.gutter));
        let summary = self.model.sessions.iter().find(|s| s.id == view.session);
        let loaded = self.model.open_of(view.session).is_some();
        let waiting = self.model.open_of(view.session).is_some_and(|o| !o.pending.is_empty());
        let draft = self.drafts.get(&view.session).filter(|d| !d.trim().is_empty()).cloned();
        let status: Option<(Hsla, String)> = match summary {
            _ if waiting => Some((theme::info(&p), "Waiting for you".to_owned())),
            Some(s) if matches!(s.run_state, RunState::Working | RunState::Starting) => {
                Some((p.secondary, s.activity.as_deref().map_or_else(|| "Working".to_owned(), super::sentence)))
            }
            _ => None,
        };
        let pane_id = pane.to_owned();
        let reply = h_flex()
            .id("reply-bar")
            .test_support()
            .h(theme::z(44.))
            .px(theme::z(16.))
            .gap(theme::z(10.))
            .items_center()
            .rounded(theme::z(theme::R_CARD + 4.))
            .border_1()
            .border_color(p.separator)
            .bg(p.raised)
            .cursor_text()
            .hover(|d| d.border_color(p.accent.opacity(0.4)))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                this.focus_chat_pane(&pane_id, cx);
                this.focus_composer_next = true;
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_size(theme::z(14.))
                    .text_color(if draft.is_some() { p.label } else { p.tertiary })
                    .child(draft.unwrap_or_else(|| "Reply…".to_owned())),
            )
            .children(status.map(|(ink, what)| div().flex_shrink_0().max_w(px(260.)).truncate().set(Type::Callout).text_color(ink).child(what)));
        let body: AnyElement = if loaded {
            self.transcript_el_for(view.scroller.clone(), view.session, cx).size_full().into_any_element()
        } else {
            h_flex().size_full().justify_center().items_center().set(Type::Callout).text_color(p.tertiary).child("Loading…").into_any_element()
        };
        v_flex()
            .size_full()
            .children(self.workspace_tabs_for(view.session, cx))
            .child(div().flex_1().min_h_0().relative().child(body))
            .child(h_flex().justify_center().pb_4().pt_1().child(column().child(reply)))
            .into_any_element()
    }
}
