//! drawing surfaces: live ui an agent draws (see `tessel_proto::surface`).
//!
//! every element is built from the design system's own pieces (type styles,
//! `theme::button`, `theme::tray`, `theme::badge`, the radius scale, `z`), so
//! whatever an agent builds sits at the same sizes as the rest of the window
//! and follows the interface size. the agent picks structure and tone; sizes,
//! spacing and colours are ours. clicks and picks go back through the daemon.

use super::*;
use crate::theme::{ButtonKind, z};
use gpui_kit::component::Icon;
use serde_json::Value;

fn s<'a>(p: &'a Value, k: &str) -> Option<&'a str> {
    p.get(k).and_then(Value::as_str)
}

/// a value as text: strings as they are, numbers and the rest as json.
fn shown(v: Option<&Value>) -> String {
    v.map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned)).unwrap_or_default()
}

impl Shell {
    pub(crate) fn prepare_asks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(open) = self.model.open.as_ref() else { return };
        let mut wanted = Vec::new();
        for n in 0..open.len() {
            let Some((item, _)) = open.row(n) else { continue };
            let ItemBody::Surface { surface, node, .. } = &item.body else { continue };
            let p = node.get("p").cloned().unwrap_or(Value::Null);
            if s(node, "k") != Some("form") || p.get("answered").is_some() || p.get("closed").is_some() {
                continue;
            }
            for f in p.get("fields").and_then(Value::as_array).into_iter().flatten() {
                if matches!(s(f, "type").unwrap_or("text"), "text" | "long") {
                    wanted.push((surface.clone(), s(f, "id").unwrap_or("").to_owned(), s(f, "placeholder").unwrap_or("").to_owned()));
                }
            }
        }
        for (surface, field, hint) in wanted {
            if self.ask_inputs.borrow().contains_key(&(surface.clone(), field.clone())) {
                continue;
            }
            let state = cx.new(|cx| gpui_kit::component::input::InputState::new(window, cx).placeholder(hint));
            self.ask_inputs.borrow_mut().insert((surface, field), state);
        }
    }

    fn form_el(&self, surface: &str, session: SessionId, p: &Value) -> AnyElement {
        let pal = self.palette.get();
        let fields: Vec<Value> = p.get("fields").and_then(Value::as_array).cloned().unwrap_or_default();
        let title = s(p, "title").map(str::to_owned);
        let card = v_flex().gap(z(12.)).p(z(16.)).rounded(px(theme::R_CARD)).border_1().border_color(pal.separator).bg(pal.raised);
        let label_of = |f: &Value| s(f, "label").unwrap_or("").to_owned();
        if p.get("answered").and_then(Value::as_bool) == Some(true) {
            let answers = p.get("answers").cloned().unwrap_or(Value::Null);
            let mut rows = v_flex().gap(z(6.));
            for f in &fields {
                let id = s(f, "id").unwrap_or("");
                let a = match answers.get(id) {
                    Some(Value::Array(v)) => v.iter().map(|x| shown(Some(x))).collect::<Vec<_>>().join(", "),
                    Some(Value::Bool(b)) => if *b { "Yes".into() } else { "No".into() },
                    other => shown(other),
                };
                if a.is_empty() {
                    continue;
                }
                rows = rows.child(h_flex().gap(z(10.)).items_start().child(div().w(z(140.)).flex_shrink_0().set(Type::Callout).text_color(pal.secondary).child(label_of(f))).child(div().flex_1().min_w_0().set(Type::Callout).text_color(pal.label).child(a)));
            }
            return card
                .child(h_flex().gap(z(8.)).child(Icon::empty().path("icons/circle-check.svg").size(z(14.)).text_color(pal.finished)).child(div().set(Type::Headline).text_color(pal.label).child(title.unwrap_or_else(|| "Answered".into()))))
                .child(rows)
                .into_any_element();
        }
        if p.get("closed").is_some() {
            return div().set(Type::Callout).text_color(pal.tertiary).child(format!("{} (no longer waiting)", title.unwrap_or_else(|| "A question".into()))).into_any_element();
        }
        let drafts = self.ask_drafts.clone();
        let inputs = self.ask_inputs.clone();
        let key = |field: &str| (surface.to_owned(), field.to_owned());
        let mut body = v_flex().gap(z(14.));
        for f in &fields {
            let id = s(f, "id").unwrap_or("").to_owned();
            let kind = s(f, "type").unwrap_or("text");
            let optional = f.get("optional").and_then(Value::as_bool).unwrap_or(false);
            let label = h_flex()
                .gap(z(6.))
                .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).text_color(pal.label).child(label_of(f)))
                .when(optional, |d| d.child(div().set(Type::Subheadline).text_color(pal.tertiary).child("optional")));
            let current = drafts.borrow().get(&key(&id)).cloned();
            let control: AnyElement = match kind {
                "select" | "multiselect" => {
                    let multi = kind == "multiselect";
                    let mut row = h_flex().flex_wrap().gap(z(6.));
                    for (n, o) in f.get("options").and_then(Value::as_array).into_iter().flatten().enumerate() {
                        let text = shown(Some(o));
                        let on = match &current {
                            Some(Value::Array(a)) => a.iter().any(|x| shown(Some(x)) == text),
                            Some(v) => shown(Some(v)) == text,
                            None => false,
                        };
                        let (drafts, k, t) = (drafts.clone(), key(&id), text.clone());
                        row = row.child(
                            div()
                                .id(SharedString::from(format!("surface-{surface}-{id}-{n}")))
                                .test_support()
                                .px(z(12.))
                                .h(z(30.))
                                .flex()
                                .items_center()
                                .rounded(px(theme::R_TILE))
                                .border_1()
                                .border_color(if on { pal.accent } else { pal.separator })
                                .when(on, |d| d.bg(pal.accent.opacity(0.12)).text_color(pal.accent))
                                .when(!on, |d| d.text_color(pal.label).hover(|d| d.bg(pal.hover)))
                                .cursor_pointer()
                                .set(Type::Callout)
                                .child(text)
                                .on_click(move |_, window, _| {
                                    let mut all = drafts.borrow_mut();
                                    if multi {
                                        let mut list: Vec<Value> = all.get(&k).and_then(Value::as_array).cloned().unwrap_or_default();
                                        match list.iter().position(|x| shown(Some(x)) == t) {
                                            Some(at) => {
                                                list.remove(at);
                                            }
                                            None => list.push(Value::String(t.clone())),
                                        }
                                        all.insert(k.clone(), Value::Array(list));
                                    } else {
                                        all.insert(k.clone(), Value::String(t.clone()));
                                    }
                                    window.refresh();
                                }),
                        );
                    }
                    row.into_any_element()
                }
                "toggle" => {
                    let on = current.as_ref().and_then(Value::as_bool).unwrap_or(false);
                    let (drafts, k) = (drafts.clone(), key(&id));
                    gpui_kit::component::switch::Switch::new(SharedString::from(format!("surface-{surface}-{id}")))
                        .checked(on)
                        .on_click(move |on: &bool, window, _| {
                            drafts.borrow_mut().insert(k.clone(), Value::Bool(*on));
                            window.refresh();
                        })
                        .into_any_element()
                }
                _ => match inputs.borrow().get(&key(&id)) {
                    Some(state) => div().id(SharedString::from(format!("surface-{surface}-{id}"))).test_support().child(gpui_kit::component::input::Input::new(state)).into_any_element(),
                    None => div().into_any_element(),
                },
            };
            body = body.child(v_flex().gap(z(6.)).child(label).child(control));
        }
        let missing = drafts.borrow().get(&key("__missing")).map(|v| shown(Some(v)));
        let bridge = self.bridge.cmds.clone();
        let (surface_name, fields_for_send) = (surface.to_owned(), fields.clone());
        let send = move |window: &mut Window, cx: &mut App| {
            let mut answers = serde_json::Map::new();
            let mut lacking = Vec::new();
            for f in &fields_for_send {
                let id = s(f, "id").unwrap_or("").to_owned();
                let k = (surface_name.clone(), id.clone());
                let value = match inputs.borrow().get(&k) {
                    Some(state) => Some(state.read(cx).value().trim().to_string()).filter(|t| !t.is_empty()).map(Value::String),
                    None => drafts.borrow().get(&k).cloned().filter(|v| !(v.is_null() || v.as_array().is_some_and(Vec::is_empty))),
                };
                match value {
                    Some(v) => {
                        answers.insert(id, v);
                    }
                    None if s(f, "type") == Some("toggle") => {
                        answers.insert(id, Value::Bool(false));
                    }
                    None if !f.get("optional").and_then(Value::as_bool).unwrap_or(false) => lacking.push(s(f, "label").unwrap_or("").to_owned()),
                    None => {}
                }
            }
            if !lacking.is_empty() {
                drafts.borrow_mut().insert((surface_name.clone(), "__missing".into()), Value::String(format!("{} still needed", lacking.join(", "))));
                window.refresh();
                return;
            }
            let _ = bridge.send(Cmd::AskAnswer { session, surface: surface_name.clone(), answers: Value::Object(answers) });
        };
        card.child(div().set(Type::Headline).text_color(pal.label).child(title.unwrap_or_else(|| "A few questions".into())))
            .children(s(p, "note").map(|n| div().set(Type::Callout).text_color(pal.secondary).child(n.to_owned())))
            .child(body)
            .child(
                h_flex()
                    .gap(z(10.))
                    .child(div().flex_1().set(Type::Callout).text_color(pal.failed).children(missing))
                    .child(theme::button(&pal, SharedString::from(format!("surface-{surface}-submit")), s(p, "submit").unwrap_or("Continue").to_owned(), None, ButtonKind::Primary).test_support().on_click(move |_, window, cx| send(window, cx))),
            )
            .into_any_element()
    }

    /// a surface, or nothing for a removed one.
    pub(crate) fn surface_el(&self, surface: &str, node: &Value, cx: &App) -> Option<AnyElement> {
        if node.is_null() {
            return None;
        }
        let session = self.model.open.as_ref().map(|o| o.id)?;
        Some(self.element(surface, session, node, cx))
    }

    /// keep `surfaces_docked` in step with the layout. when it flips, pane
    /// surfaces come into or leave the chat, so its rows are measured again.
    pub(crate) fn sync_surfaces_dock(&mut self, cx: &mut Context<Self>) {
        let docked = self.ui.panes().into_iter().any(|p| p.module == Some(crate::ui::Module::Surfaces) && self.pane_open(p));
        if docked != self.surfaces_docked {
            self.surfaces_docked = docked;
            let len = self.model.open.as_ref().map_or(0, |o| o.len());
            self.scroller.update(cx, |s, cx| s.reset(len, cx));
        }
    }

    /// the Surfaces pane: what the agent drew for it, newest at the bottom.
    pub(crate) fn surfaces_pane(&self, cx: &App) -> AnyElement {
        let pal = self.palette.get();
        let drawn: Vec<AnyElement> = self
            .model
            .open
            .as_ref()
            .into_iter()
            .flat_map(|o| (0..o.len()).filter_map(|i| o.row(i)))
            .filter_map(|(item, _)| match &item.body {
                ItemBody::Surface { surface, node, region } if region == "pane" => self.surface_el(surface, node, cx),
                _ => None,
            })
            .collect();
        if drawn.is_empty() {
            return v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap(z(6.))
                .p(z(24.))
                .child(Icon::empty().path("icons/layout-dashboard.svg").size(z(20.)).text_color(pal.tertiary))
                .child(div().set(Type::Callout).text_color(pal.secondary).child("Nothing here yet"))
                .child(div().set(Type::Subheadline).text_color(pal.tertiary).text_center().max_w(z(240.)).child("Ask the agent to show progress, results or choices in this pane."))
                .into_any_element();
        }
        v_flex().id("surfaces-pane").test_support().size_full().overflow_y_scroll().p(z(12.)).gap(z(12.)).children(drawn).into_any_element()
    }

    fn tone(&self, p: &Value, default: Hsla) -> Hsla {
        let pal = self.palette.get();
        match s(p, "tone") {
            Some("muted") => pal.secondary,
            Some("faint") => pal.tertiary,
            Some("accent") => pal.accent,
            Some("success") => pal.finished,
            Some("warning") => pal.blocked,
            Some("error") => pal.failed,
            _ => default,
        }
    }

    /// a status tone (success, warning, error, info), if the props name one.
    fn status_tone(&self, p: &Value) -> Option<(Hsla, bool)> {
        let pal = self.palette.get();
        match s(p, "tone") {
            Some("success") => Some((pal.finished, false)),
            Some("warning") => Some((pal.blocked, false)),
            Some("error") => Some((pal.failed, true)),
            Some("info") => Some((theme::info(&pal), false)),
            _ => None,
        }
    }

    /// orchard's status badge: the dot and the word, colour never alone, and
    /// only a failure in semibold. other tones stay a quiet pill.
    fn badge(&self, text: String, p: &Value) -> AnyElement {
        let pal = self.palette.get();
        match self.status_tone(p) {
            Some((c, failing)) => h_flex()
                .h(z(22.))
                .px(z(8.))
                .gap(z(6.))
                .rounded_full()
                .bg(pal.label.opacity(0.06))
                .set(Type::Callout)
                .child(status_dot(c, false, &pal))
                .child(div().text_color(if failing { pal.failed } else { pal.label }).when(failing, |d| d.font_weight(FontWeight::SEMIBOLD)).child(text))
                .into_any_element(),
            None => theme::badge(text, self.tone(p, pal.secondary)).into_any_element(),
        }
    }

    fn element(&self, surface: &str, session: SessionId, node: &Value, cx: &App) -> AnyElement {
        let pal = self.palette.get();
        let k = s(node, "k").unwrap_or("");
        let id = s(node, "id").unwrap_or("").to_owned();
        let p = node.get("p").cloned().unwrap_or(Value::Null);
        let kids = || -> Vec<AnyElement> { node.get("c").and_then(Value::as_array).into_iter().flatten().map(|c| self.element(surface, session, c, cx)).collect() };
        let gap = |default: f32| z(p.get("gap").and_then(Value::as_f64).map_or(default, |g| g.clamp(0.0, 32.0) as f32));
        let ev = |element: String, act: &'static str, value: Value| {
            let bridge = self.bridge.cmds.clone();
            let surface = surface.to_owned();
            move || {
                let _ = bridge.send(Cmd::SurfaceEvent { session, surface: surface.clone(), element: element.clone(), act: act.to_owned(), value: value.clone() });
            }
        };
        let eid = |suffix: &str| SharedString::from(format!("surface-{surface}-{id}{suffix}"));
        // controls keep their own width: in a column they don't stretch across it.
        let natural = |el: AnyElement| h_flex().child(el).into_any_element();
        match k {
            "column" => v_flex().gap(gap(10.)).children(kids()).into_any_element(),
            // a row that doesn't fit wraps rather than pushing its end out of the card.
            "row" => h_flex().items_center().flex_wrap().gap(gap(8.)).children(kids()).into_any_element(),
            "card" => {
                let title = s(&p, "title").map(str::to_owned);
                let note = s(&p, "note").map(str::to_owned);
                let status = s(&p, "status").map(|t| self.badge(t.to_owned(), &p));
                let head = (title.is_some() || note.is_some() || status.is_some()).then(|| {
                    h_flex()
                        .items_start()
                        .gap_3()
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap(z(2.))
                                .children(title.map(|t| div().set(Type::Headline).text_color(pal.label).child(t)))
                                .children(note.map(|t| div().set(Type::Callout).text_color(pal.tertiary).child(t))),
                        )
                        .children(status)
                });
                v_flex()
                    .p(z(16.))
                    .gap(z(14.))
                    .rounded(z(theme::R_CARD))
                    .border_1()
                    .border_color(pal.separator)
                    .bg(pal.raised)
                    .children(head)
                    .children(kids())
                    .into_any_element()
            }
            "text" => {
                let style = match s(&p, "size") {
                    Some("title") => Type::Title,
                    Some("headline") => Type::Headline,
                    Some("callout") => Type::Callout,
                    Some("caption") => Type::Subheadline,
                    _ if p.get("mono").and_then(Value::as_bool) == Some(true) => Type::Code,
                    _ => Type::Body,
                };
                let weight = match s(&p, "weight") {
                    Some("medium") => FontWeight::MEDIUM,
                    Some("semibold") => FontWeight::SEMIBOLD,
                    _ => style.base().2,
                };
                // a status line (done, warning, failed): a mark in its own column,
                // the words in ink, as the working lines beside it are drawn. a
                // tick or bullet the agent typed itself is dropped for the mark.
                if s(&p, "size").is_none()
                    && let Some((ink, failing)) = self.status_tone(&p)
                {
                    let text = shown(p.get("text"));
                    let text = text.trim_start_matches(['✓', '✔', '✅', '✗', '✘', '❌', '•', '-', '*', '⚠', '️']).trim_start().to_owned();
                    let mark = match s(&p, "tone") {
                        Some("success") => Icon::empty().path("icons/check.svg").size(z(13.)).text_color(ink).into_any_element(),
                        Some("error") => Icon::empty().path("icons/x.svg").size(z(13.)).text_color(ink).into_any_element(),
                        _ => status_dot(ink, false, &pal).into_any_element(),
                    };
                    return h_flex()
                        .gap(z(8.))
                        .items_start()
                        .child(h_flex().w(z(18.)).h(z(20.)).flex_shrink_0().justify_center().items_center().child(mark))
                        .child(div().flex_1().min_w_0().set(Type::Callout).font_weight(if failing { FontWeight::MEDIUM } else { weight }).text_color(pal.label).child(text))
                        .into_any_element();
                }
                div().set(style).font_weight(weight).text_color(self.tone(&p, pal.label)).child(shown(p.get("text"))).into_any_element()
            }
            "markdown" => div()
                .set(Type::Prose)
                .child(TextView::markdown(ElementId::from(eid("-md")), s(&p, "text").unwrap_or("").to_owned()).selectable(true).style(theme::prose(&pal)))
                .into_any_element(),
            "code" => div()
                .px(z(12.))
                .py(z(10.))
                .rounded(z(theme::R_TILE))
                .bg(pal.fill)
                .set(Type::Code)
                .child(s(&p, "text").unwrap_or("").to_owned())
                .into_any_element(),
            "button" => {
                let kind = match s(&p, "variant") {
                    Some("primary") => ButtonKind::Primary,
                    Some("ghost") => ButtonKind::Ghost,
                    _ => ButtonKind::Secondary,
                };
                let send = ev(id.clone(), "click", Value::Null);
                let label = s(&p, "label").unwrap_or("").to_owned();
                natural(theme::button(&pal, eid(""), label.clone(), s(&p, "icon"), kind).test_support().aria_label(label).on_click(move |_, _, _| send()).into_any_element())
            }
            "form" => self.form_el(surface, session, &p),
            "badge" => natural(self.badge(shown(p.get("text")), &p)),
            "progress" => {
                let value = p.get("value").and_then(Value::as_f64).map(|v| v.clamp(0.0, 1.0) as f32);
                let fill = self.tone(&p, pal.accent);
                let track = div().h(z(4.)).w_full().rounded_full().bg(pal.label.opacity(0.08)).overflow_hidden();
                // still going: orchard's live dot by the label. done: nothing moves.
                let running = value.is_none_or(|v| v < 1.0);
                v_flex()
                    .gap(z(8.))
                    .child(
                        h_flex()
                            .gap(z(8.))
                            .set(Type::Callout)
                            .when(running, |d| d.child(live_dot(pal.blocked, &pal)))
                            .child(div().flex_1().min_w_0().truncate().text_color(pal.label).child(s(&p, "label").unwrap_or("").to_owned()))
                            .children(value.map(|v| div().set(Type::Code).text_color(pal.secondary).child(format!("{:.0}%", v * 100.0)))),
                    )
                    // unknown amount: the track alone; the dot already says it's moving.
                    .child(match value {
                        Some(v) => track.child(div().h_full().w(relative(v)).rounded_full().bg(fill)),
                        None => track,
                    })
                    .into_any_element()
            }
            // a working line: orchard's orbiting dot, not a shimmer.
            "shimmer" => h_flex()
                .gap(z(8.))
                .items_start()
                .child(h_flex().w(z(18.)).h(z(20.)).flex_shrink_0().justify_center().items_center().child(live_dot(pal.blocked, &pal)))
                .child(div().flex_1().min_w_0().set(Type::Callout).text_color(pal.label).child(s(&p, "text").unwrap_or("Working").to_owned()))
                .into_any_element(),
            "checkbox" => {
                let checked = p.get("checked").and_then(Value::as_bool).unwrap_or(false);
                let send = ev(id.clone(), "change", Value::Bool(!checked));
                gpui_kit::component::checkbox::Checkbox::new(ElementId::from(eid("")))
                    .checked(checked)
                    .label(s(&p, "label").unwrap_or("").to_owned())
                    .on_click(move |_, _, _| send())
                    .into_any_element()
            }
            "choice" => {
                let value = s(&p, "value").map(str::to_owned);
                let mut tray = theme::tray(&pal);
                for (i, opt) in p.get("options").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).enumerate() {
                    let send = ev(id.clone(), "change", Value::String(opt.to_owned()));
                    let on = value.as_deref() == Some(opt);
                    tray = tray.child(
                        theme::segment(&pal, eid(&format!("-{i}")), opt.to_owned(), on)
                            .test_support()
                            .aria_label(SharedString::from(format!("{opt}{}", if on { ", selected" } else { "" })))
                            .on_click(move |_, _, _| send()),
                    );
                }
                natural(tray.into_any_element())
            }
            // the bold budget: a number is medium, in ink; its status is a dot by
            // the label, and only a failure turns the number red and semibold.
            "metric" => {
                let tone = self.status_tone(&p);
                let failing = tone.is_some_and(|(_, f)| f);
                v_flex()
                    .gap(z(4.))
                    .child(
                        h_flex()
                            .gap(z(6.))
                            .children(tone.map(|(c, _)| status_dot(c, false, &pal)))
                            .child(div().set(Type::Subheadline).text_color(pal.secondary).child(s(&p, "label").unwrap_or("").to_owned())),
                    )
                    .child(
                        h_flex()
                            .gap(z(6.))
                            .items_baseline()
                            .child(
                                div()
                                    .set(Type::Title)
                                    .font_weight(if failing { FontWeight::SEMIBOLD } else { FontWeight::MEDIUM })
                                    .text_color(if failing { pal.failed } else { pal.label })
                                    .child(shown(p.get("value"))),
                            )
                            .children(s(&p, "delta").map(|d| div().set(Type::Subheadline).text_color(pal.tertiary).child(d.to_owned()))),
                    )
                    .into_any_element()
            }
            "list" => {
                let mut col = v_flex().gap(px(1.));
                for (i, item) in p.get("items").and_then(Value::as_array).into_iter().flatten().enumerate() {
                    let send = (!id.is_empty()).then(|| ev(id.clone(), "click", Value::from(i)));
                    col = col.child(
                        h_flex()
                            .id(ElementId::from(eid(&format!("-{i}"))))
                            .min_h(z(theme::CONTROL_H + 4.))
                            .px(z(8.))
                            .gap(z(10.))
                            .rounded(z(theme::R_TILE))
                            .when(send.is_some(), |d| d.cursor_pointer().hover(|d| d.bg(pal.hover)))
                            .when_some(send, |d, send| d.on_click(move |_, _, _| send()))
                            .children(s(item, "icon").map(|ic| Icon::empty().path(format!("icons/{ic}.svg")).size(z(14.)).text_color(pal.secondary)))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().set(Type::Body).truncate().child(s(item, "title").unwrap_or("").to_owned()))
                                    .children(s(item, "note").map(|n| div().set(Type::Subheadline).text_color(pal.tertiary).truncate().child(n.to_owned()))),
                            ),
                    );
                }
                col.into_any_element()
            }
            "kv" => {
                let mut col = v_flex().gap(z(6.));
                for row in p.get("rows").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_array) {
                    col = col.child(
                        h_flex()
                            .gap_3()
                            .set(Type::Callout)
                            .child(div().w(z(120.)).flex_shrink_0().text_color(pal.tertiary).truncate().child(shown(row.first())))
                            .child(div().flex_1().min_w_0().text_color(pal.label).child(shown(row.get(1)))),
                    );
                }
                col.into_any_element()
            }
            "icon" => Icon::empty().path(format!("icons/{}.svg", s(&p, "name").unwrap_or("circle"))).size(z(16.)).text_color(self.tone(&p, pal.secondary)).into_any_element(),
            "divider" => div().h(px(1.)).w_full().bg(pal.separator).into_any_element(),
            "spacer" => div().flex_1().into_any_element(),
            // the daemon refuses unknown elements; one drawn by an older build shows as nothing.
            _ => div().into_any_element(),
        }
    }
}
