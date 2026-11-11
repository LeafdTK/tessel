//! the composer's menus: `/` for the agent's commands and skills, `@` for the
//! session's files. they follow what's being typed at the end of the message,
//! ranked as the palette ranks, and a pick replaces what was typed.

use super::*;
use gpui_kit::component::Icon;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mention {
    Command,
    File,
}

/// one row: what it inserts, what it shows.
#[derive(Clone, Debug)]
pub(crate) struct Choice {
    pub insert: String,
    pub title: String,
    pub note: String,
}

/// what's being typed that a menu answers: a `/command` at the start of the
/// message, or an `@path` as its last word. the query, and where it starts.
pub(crate) fn mention_at_end(text: &str) -> Option<(Mention, String, usize)> {
    if text.ends_with(char::is_whitespace) {
        return None;
    }
    let start = text.rfind(char::is_whitespace).map_or(0, |i| i + 1);
    let word = &text[start..];
    if let Some(q) = word.strip_prefix('/')
        && start == 0
    {
        return Some((Mention::Command, q.to_owned(), start));
    }
    word.strip_prefix('@').map(|q| (Mention::File, q.to_owned(), start))
}

impl Shell {
    /// the menu for what's being typed, best first; none when nothing fits.
    pub(crate) fn mention_choices(&self, cx: &App) -> Option<(Mention, Vec<Choice>)> {
        let text = self.composer.read(cx).value().to_string();
        let (kind, query, _) = mention_at_end(&text)?;
        if self.mention_dismissed.as_deref() == Some(text.as_str()) {
            return None;
        }
        let score = |s: &str| crate::commands::score(&query, s);
        let mut found: Vec<(i32, Choice)> = match kind {
            Mention::Command => self
                .slash_commands
                .iter()
                .filter_map(|c| {
                    let sc = score(&c.name)?;
                    let note = if c.hint.is_empty() { c.description.clone() } else { format!("{}  {}", c.hint, c.description) };
                    Some((sc, Choice { insert: format!("/{} ", c.name), title: format!("/{}", c.name), note }))
                })
                .collect(),
            Mention::File => self
                .palette_ui
                .files
                .iter()
                .filter_map(|path| {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    let sc = score(name).map(|s| s + 30).or_else(|| score(path))?;
                    let dir = path.rsplit_once('/').map(|(d, _)| d.to_owned()).unwrap_or_default();
                    Some((sc - path.len() as i32 / 8, Choice { insert: format!("@{path} "), title: name.to_owned(), note: dir }))
                })
                .collect(),
        };
        if found.is_empty() {
            return None;
        }
        if !query.is_empty() {
            found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.title.cmp(&b.1.title)));
        }
        found.truncate(8);
        Some((kind, found.into_iter().map(|(_, c)| c).collect()))
    }

    /// put a choice in place of what was typed.
    pub(crate) fn accept_mention(&mut self, choice: &Choice, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.composer.read(cx).value().to_string();
        let Some((_, _, start)) = mention_at_end(&text) else { return };
        let next = format!("{}{}", &text[..start], choice.insert);
        let last = next.split('\n').count().saturating_sub(1) as u32;
        self.composer.update(cx, |s, cx| {
            s.set_value(next, window, cx);
            // a multi-line box puts the caret back at the start: carry on typing after the pick.
            s.set_cursor_position(gpui_kit::component::input::Position { line: last, character: u32::MAX }, window, cx);
        });
        self.mention_sel = 0;
        cx.notify();
    }

    /// keys while a menu is up: arrows choose, return or tab takes, escape closes.
    /// true when the key was the menu's.
    pub(crate) fn mention_key(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some((_, choices)) = self.mention_choices(cx) else { return false };
        match key {
            "up" => self.mention_sel = (self.mention_sel + choices.len() - 1) % choices.len(),
            "down" => self.mention_sel = (self.mention_sel + 1) % choices.len(),
            "enter" | "tab" => {
                let c = choices[self.mention_sel.min(choices.len() - 1)].clone();
                self.accept_mention(&c, window, cx);
            }
            "escape" => self.mention_dismissed = Some(self.composer.read(cx).value().to_string()),
            _ => return false,
        }
        cx.notify();
        true
    }

    /// the files, asked for the first time an `@` is typed in a session.
    pub(crate) fn want_files(&mut self) {
        if let Some(session) = self.ws_session
            && self.palette_ui.files_for != Some(session)
        {
            self.palette_ui.files_for = Some(session);
            self.cmd(Cmd::Ask { tag: "palette.files", req: tessel_proto::Request::FsIndex { session } });
        }
    }

    /// the menu, drawn above the composer.
    pub(crate) fn mention_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (kind, choices) = self.mention_choices(cx)?;
        let p = self.palette.get();
        let sel = self.mention_sel.min(choices.len() - 1);
        let mut list = theme::float_list(&p).id("mention-list").test_support().max_h(px(300.)).overflow_y_scroll();
        list = list.child(theme::float_label(&p, if kind == Mention::Command { "Commands and skills" } else { "Files" }));
        for (i, c) in choices.into_iter().enumerate() {
            let pick = c.clone();
            list = list.child(
                h_flex()
                    .id(SharedString::from(format!("mention-{i}")))
                    .test_support()
                    .min_h(theme::z(32.))
                    .px(theme::z(10.))
                    .gap(theme::z(10.))
                    .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                    .cursor_pointer()
                    .when(i == sel, |d| d.bg(p.hover))
                    .on_click(cx.listener(move |this, _, window, cx| this.accept_mention(&pick, window, cx)))
                    .child(Icon::empty().path(if kind == Mention::Command { "icons/slash.svg" } else { "icons/file.svg" }).size(theme::z(14.)).text_color(p.tertiary))
                    .child(div().flex_shrink_0().set(if kind == Mention::Command { Type::Code } else { Type::Body }).text_color(p.label).child(c.title))
                    .child(div().flex_1().min_w_0().truncate().set(Type::Subheadline).text_color(p.tertiary).child(c.note)),
            );
        }
        Some(
            div()
                .absolute()
                .bottom_full()
                .left_0()
                .right_0()
                .mb(theme::z(8.))
                .rounded(theme::z(theme::R_FLOAT))
                .border_1()
                .border_color(p.separator)
                .bg(p.raised)
                .shadow_lg()
                .overflow_hidden()
                .child(list)
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{Mention, mention_at_end};

    #[test]
    fn a_menu_answers_only_what_is_being_typed() {
        assert_eq!(mention_at_end("/rev"), Some((Mention::Command, "rev".into(), 0)));
        assert_eq!(mention_at_end("fix @src/ma"), Some((Mention::File, "src/ma".into(), 4)));
        assert_eq!(mention_at_end("a/b"), None, "a slash inside a word is a path");
        assert_eq!(mention_at_end("look /tmp"), None, "a command only starts the message");
        assert_eq!(mention_at_end("done @x "), None, "a finished word wants no menu");
        assert_eq!(mention_at_end(""), None);
    }
}
