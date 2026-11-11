//! drawing surfaces: live ui an agent draws (see `photon_proto::surface`).
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
                let status = s(&p, "status").map(|t| theme::badge(t.to_owned(), self.tone(&p, pal.secondary)));
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
            "badge" => natural(theme::badge(shown(p.get("text")), self.tone(&p, pal.secondary)).into_any_element()),
            "progress" => {
                let value = p.get("value").and_then(Value::as_f64).map(|v| v.clamp(0.0, 1.0) as f32);
                let fill = self.tone(&p, pal.accent);
                let track = div().h(z(4.)).w_full().rounded_full().bg(pal.fill).overflow_hidden();
                v_flex()
                    .gap(z(6.))
                    .when(s(&p, "label").is_some() || value.is_some(), |d| {
                        d.child(
                            h_flex()
                                .set(Type::Callout)
                                .child(div().flex_1().min_w_0().truncate().text_color(pal.secondary).child(s(&p, "label").unwrap_or("").to_owned()))
                                .children(value.map(|v| div().text_color(pal.tertiary).child(format!("{:.0}%", v * 100.0)))),
                        )
                    })
                    .child(match value {
                        Some(v) => track.child(div().h_full().w(relative(v)).rounded_full().bg(fill)),
                        // nothing known yet: a sweep.
                        None => track.child(gpui_kit::component::skeleton::Skeleton::new().h_full().w_full().rounded_full()),
                    })
                    .into_any_element()
            }
            "shimmer" => h_flex()
                .gap_2()
                .child(gpui_kit::component::spinner::Spinner::new().xsmall().color(pal.tertiary))
                .child(div().set(Type::Callout).text_color(pal.secondary).child(s(&p, "text").unwrap_or("Working…").to_owned()))
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
            "metric" => v_flex()
                .gap(z(2.))
                .child(div().set(Type::Subheadline).text_color(pal.tertiary).child(s(&p, "label").unwrap_or("").to_owned()))
                .child(
                    h_flex()
                        .gap(z(6.))
                        .items_baseline()
                        .child(div().set(Type::Title).text_color(self.tone(&p, pal.label)).child(shown(p.get("value"))))
                        .children(s(&p, "delta").map(|d| div().set(Type::Subheadline).text_color(pal.tertiary).child(d.to_owned()))),
                )
                .into_any_element(),
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
