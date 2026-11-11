//! models: the loadout (a few favourites, each with how hard it thinks and
//! whether it runs fast), the picker on the composer, and the page that edits
//! the loadout. the loadout is kept by the daemon, so every window, script and
//! agent sees the same one.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::switch::Switch;
use serde_json::json;
use tessel_proto::{EFFORTS, FavoriteModel, MAX_FAVORITES, ModelInfo};

/// a model's family, as a letter and a hue: what its tile shows.
pub(crate) fn family(name: &str) -> (char, f32) {
    let n = name.to_lowercase();
    for (word, letter, hue) in [("opus", 'O', 0.05), ("fable", 'F', 0.74), ("sonnet", 'S', 0.56), ("haiku", 'H', 0.38)] {
        if n.contains(word) {
            return (letter, hue);
        }
    }
    let letter = name.chars().find(|c| c.is_alphanumeric()).map_or('?', |c| c.to_ascii_uppercase());
    let hue = name.bytes().fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(u32::from(b))) % 360;
    (letter, hue as f32 / 360.0)
}

/// a model's mark: its family's letter on a quiet tile of the family's colour.
pub(crate) fn model_tile(name: &str, size: f32, pal: &theme::Palette) -> Div {
    let (letter, hue) = family(name);
    let ink = gpui_kit::hsla(hue, 0.5, if pal.dark { 0.7 } else { 0.4 }, 1.0);
    div()
        .size(theme::z(size))
        .flex_shrink_0()
        .flex()
        .items_center()
        .justify_center()
        .rounded(theme::z((size * 0.3).round()))
        .bg(ink.opacity(if pal.dark { 0.2 } else { 0.13 }))
        .text_color(ink)
        .text_size(theme::z((size * 0.6).round()))
        .font_weight(FontWeight::SEMIBOLD)
        .child(letter.to_string())
}

/// how an effort level reads.
pub(crate) fn effort_name(e: &str) -> &'static str {
    match e {
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra high",
        "max" => "Max",
        _ => "Default",
    }
}

fn effort_short(e: &str) -> &'static str {
    match e {
        "medium" => "Med",
        "xhigh" => "XHigh",
        other => effort_name(other),
    }
}

/// a dragged favourite, by where it was.
#[derive(Clone, Copy)]
pub(crate) struct FavoriteDrag(pub usize);

struct DragGhost(SharedString);

impl Render for DragGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().px_2().py_1().rounded(px(theme::R_TILE)).bg(gpui_kit::black().opacity(0.06)).set(Type::Callout).child(self.0.clone())
    }
}

impl Shell {
    /// the loadout: the daemon's, or the usual one before it has said.
    pub(crate) fn favorites(&self) -> Vec<FavoriteModel> {
        self.daemon_settings.as_ref().map_or_else(|| tessel_proto::DaemonSettings::default().models, |d| d.models.clone())
    }

    /// what the agent says about a model, by alias or id.
    pub(crate) fn model_info(&self, model: &str) -> Option<&ModelInfo> {
        let hit = |m: &&ModelInfo| m.value == model || (!m.id.is_empty() && m.id == model);
        self.models.iter().filter(|m| m.value != "default").find(hit).or_else(|| self.models.iter().find(hit))
    }

    fn current_model(&self) -> Option<String> {
        self.model.open_summary().and_then(|s| s.model.clone())
    }

    fn current_effort(&self) -> Option<String> {
        self.model.open_summary().and_then(|s| s.effort.clone())
    }

    fn current_fast(&self) -> bool {
        self.model.open_summary().is_some_and(|s| s.fast)
    }

    /// the effort levels a model takes; all of them while the agent hasn't said.
    fn efforts_of(&self, model: Option<&str>) -> Vec<String> {
        match model.and_then(|m| self.model_info(m)) {
            Some(info) => info.efforts.clone(),
            None => EFFORTS.iter().map(|e| (*e).to_owned()).collect(),
        }
    }

    fn fast_ok(&self, model: Option<&str>) -> bool {
        model.and_then(|m| self.model_info(m)).is_some_and(|m| m.fast)
    }

    fn current_efforts(&self) -> Vec<String> {
        self.efforts_of(self.current_model().as_deref())
    }

    fn current_supports_fast(&self) -> bool {
        self.fast_ok(self.current_model().as_deref())
    }

    /// pick a favourite: its model, its effort, its speed, all at once.
    pub(crate) fn pick_favorite(&mut self, session: SessionId, fav: &FavoriteModel, cx: &mut Context<Self>) {
        let fast = fav.fast && self.fast_ok(Some(&fav.model));
        self.cmd(Cmd::SetModel { session, model: fav.model.clone() });
        if fav.effort.is_some() {
            self.cmd(Cmd::SetEffort { session, effort: fav.effort.clone() });
        }
        self.cmd(Cmd::SetFast { session, fast });
        cx.notify();
    }

    pub(crate) fn set_effort(&mut self, session: SessionId, effort: Option<String>, cx: &mut Context<Self>) {
        self.cmd(Cmd::SetEffort { session, effort });
        cx.notify();
    }

    /// a session's next effort level up, round to the lowest again.
    pub(crate) fn next_effort(&self, session: SessionId) -> Option<String> {
        let s = self.model.summary(session)?;
        let levels = self.efforts_of(s.model.as_deref());
        let at = s.effort.as_ref().and_then(|e| levels.iter().position(|l| l == e));
        levels.get(at.map_or(0, |i| (i + 1) % levels.len())).cloned()
    }

    /// fast mode, where the session's model has it. answers whether it's on.
    pub(crate) fn set_fast(&mut self, session: SessionId, fast: bool, cx: &mut Context<Self>) -> bool {
        let ok = self.fast_ok(self.model.summary(session).and_then(|s| s.model.as_deref()));
        self.cmd(Cmd::SetFast { session, fast: fast && ok });
        cx.notify();
        fast && ok
    }

    pub(crate) fn session_fast(&self, session: SessionId) -> bool {
        self.model.summary(session).is_some_and(|s| s.fast)
    }

    /// keep a new loadout: shown at once, kept by the daemon.
    pub(crate) fn save_favorites(&mut self, favs: Vec<FavoriteModel>, cx: &mut Context<Self>) {
        if let Some(d) = self.daemon_settings.as_mut() {
            d.models = favs.clone();
        }
        self.cmd(Cmd::SetSettings(json!({"models": favs})));
        cx.notify();
    }

    pub(crate) fn move_favorite(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        let mut favs = self.favorites();
        if from >= favs.len() || to >= favs.len() || from == to {
            return;
        }
        let f = favs.remove(from);
        favs.insert(to, f);
        self.save_favorites(favs, cx);
    }

    pub(crate) fn toggle_favorite(&mut self, model: &str, cx: &mut Context<Self>) {
        let mut favs = self.favorites();
        if let Some(i) = favs.iter().position(|f| f.model == model) {
            favs.remove(i);
        } else if favs.len() < MAX_FAVORITES {
            let effort = self.model_info(model).and_then(|m| m.efforts.iter().find(|e| *e == "high").cloned());
            favs.push(FavoriteModel { model: model.to_owned(), effort, fast: false });
        } else {
            self.error = Some(format!("{MAX_FAVORITES} favourites at most: unstar one first."));
            cx.notify();
            return;
        }
        self.save_favorites(favs, cx);
    }

    fn edit_favorite(&mut self, model: &str, change: impl FnOnce(&mut FavoriteModel), cx: &mut Context<Self>) {
        let mut favs = self.favorites();
        if let Some(f) = favs.iter_mut().find(|f| f.model == model) {
            change(f);
            self.save_favorites(favs, cx);
        }
    }

    // ---- the chip and its picker ------------------------------------------------

    /// the model, as the composer shows it: its mark, its name, how hard it
    /// thinks, and whether it runs fast. opens the picker.
    pub(crate) fn model_chip(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let current = self.current_model();
        let name = current.as_deref().map_or_else(|| "Model".to_owned(), |m| self.model_name(m));
        let effort = self.current_effort();
        let fast = self.current_fast();
        let me = cx.entity().downgrade();
        let search = self.model_search.clone();
        let clear = self.model_search.clone();
        let chip_name = name.clone();
        Popover::new("model-popover")
            .p_0()
            .anchor(Anchor::BottomLeft)
            .on_open_change(move |open, window, cx| {
                if *open {
                    clear.update(cx, |s, cx| {
                        s.set_value("", window, cx);
                        s.focus(window, cx);
                    });
                }
            })
            .trigger(
                super::composer::tool_button("model-menu")
                    .tooltip("Model, effort and speed")
                    .child(
                        h_flex()
                            .gap(theme::z(6.))
                            .child(model_tile(&chip_name, 16., &p))
                            .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).text_color(p.label).child(chip_name.clone()))
                            .children(effort.as_deref().map(|e| div().set(Type::Callout).text_color(p.tertiary).child(effort_name(e))))
                            .when(fast, |d| d.child(theme::badge("Fast", p.accent)))
                            .child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(12.)).text_color(p.tertiary)),
                    ),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                shell.read(cx).model_picker(&me, &popover, &search, cx)
            })
            .into_any_element()
    }

    fn model_picker(&self, me: &WeakEntity<Shell>, popover: &Entity<gpui_kit::component::popover::PopoverState>, search: &Entity<InputState>, cx: &App) -> AnyElement {
        let p = self.palette.get();
        let query = search.read(cx).value().trim().to_lowercase();
        let current = self.current_model();
        let is_current = |m: &str| current.as_deref().is_some_and(|c| c == m || self.model_info(m).is_some_and(|i| i.id == c || i.value == c));
        let mut list = theme::float_list(&p).id("model-list").test_support().max_h(px(360.)).overflow_y_scroll();
        let dismiss = |popover: &Entity<gpui_kit::component::popover::PopoverState>, window: &mut Window, cx: &mut App| popover.update(cx, |st, cx| st.dismiss(window, cx));

        if query.is_empty() {
            // the loadout, in order, each with its effort and its key.
            for (i, fav) in self.favorites().into_iter().enumerate() {
                let name = self.model_name(&fav.model);
                let on = is_current(&fav.model);
                let (me1, me2, pop, f) = (me.clone(), me.clone(), popover.clone(), fav.clone());
                let ghost = SharedString::from(name.clone());
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("favorite-{i}")))
                        .test_support()
                        .aria_label(SharedString::from(format!("{name}{}", if on { ", current" } else { "" })))
                        .h(theme::z(theme::FLOAT_ITEM_H))
                        .px(theme::z(8.))
                        .gap(theme::z(8.))
                        .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                        .on_drag(FavoriteDrag(i), move |_, _, _, cx| cx.new(|_| DragGhost(ghost.clone())))
                        .on_drop(move |from: &FavoriteDrag, _, cx| {
                            let _ = me1.update(cx, |this, cx| this.move_favorite(from.0, i, cx));
                        })
                        .on_click(move |_, window, cx| {
                            let _ = me2.update(cx, |this, cx| if let Some(id) = this.open_session() { this.pick_favorite(id, &f, cx) });
                            dismiss(&pop, window, cx);
                        })
                        .child(Icon::empty().path("icons/grip-vertical.svg").size(theme::z(12.)).text_color(p.tertiary))
                        .child(model_tile(&name, 18., &p))
                        .child(div().set(Type::Body).text_color(p.label).child(name))
                        .children(fav.effort.as_deref().map(|e| div().set(Type::Callout).text_color(p.tertiary).child(effort_name(e))))
                        .when(fav.fast, |d| d.child(theme::badge("Fast", p.accent)))
                        .child(div().flex_1())
                        .child(theme::keycaps(&p, &format!("⌃ ⌘ {}", i + 1)))
                        .when(on, |d| d.child(Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.accent))),
                );
            }
        } else {
            // a search: every model the agent offers.
            let found: Vec<ModelInfo> = self.models.iter().filter(|m| m.value != "default").filter(|m| m.name.to_lowercase().contains(&query) || m.description.to_lowercase().contains(&query)).cloned().collect();
            if found.is_empty() {
                list = list.child(div().px_2().py_2().set(Type::Callout).text_color(p.secondary).child(if self.models.is_empty() { "Asking the agent which models it has…" } else { "No model matches." }));
            }
            for m in found {
                let on = is_current(&m.value);
                let (me, pop, value) = (me.clone(), popover.clone(), m.value.clone());
                list = list.child(
                    h_flex()
                        .id(SharedString::from(format!("model-{}", m.value)))
                        .test_support()
                        .aria_label(SharedString::from(format!("{}{}", m.name, if on { ", current" } else { "" })))
                        .min_h(theme::z(theme::FLOAT_ITEM_H))
                        .px(theme::z(8.))
                        .gap(theme::z(10.))
                        .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                        .cursor_pointer()
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                        .on_click(move |_, window, cx| {
                            let _ = me.update(cx, |this, cx| this.set_model(value.clone(), cx));
                            dismiss(&pop, window, cx);
                        })
                        .child(model_tile(&m.name, 18., &p))
                        .child(v_flex().flex_1().min_w_0().child(div().set(Type::Body).child(m.name.clone())).child(div().set(Type::Subheadline).text_color(p.tertiary).truncate().child(m.description.clone()))),
                );
            }
        }

        // how the current model runs: its effort, and fast if it has it.
        let levels = self.current_efforts();
        let effort = self.current_effort();
        let mut tray = theme::tray(&p);
        for level in &levels {
            let (me, l) = (me.clone(), level.clone());
            tray = tray.child(
                theme::segment(&p, SharedString::from(format!("effort-{level}")), effort_short(level), effort.as_deref() == Some(level.as_str()))
                    .test_support()
                    .aria_label(SharedString::from(effort_name(level)))
                    .on_click(move |_, _, cx| {
                        let _ = me.update(cx, |this, cx| if let Some(id) = this.open_session() { this.set_effort(id, Some(l.clone()), cx) });
                    }),
            );
        }
        let fast = self.current_fast();
        let row = |label: &'static str, keys: &'static str| h_flex().px(theme::z(16.)).h(theme::z(40.)).gap(theme::z(10.)).child(div().flex_1().set(Type::Body).text_color(p.label).child(label)).child(theme::keycaps(&p, keys));
        let (me_fast, me_edit, pop_edit) = (me.clone(), me.clone(), popover.clone());
        v_flex()
            .w(theme::z(380.))
            .child(
                h_flex()
                    .h(theme::z(42.))
                    .px(theme::z(16.))
                    .gap_2()
                    .border_b_1()
                    .border_color(p.separator)
                    .child(div().text_color(p.secondary).child(Icon::empty().path("icons/search.svg").small()))
                    .child(div().flex_1().child(Input::new(search).id("model-search").appearance(false))),
            )
            .child(list)
            .child(div().h(px(1.)).bg(p.separator))
            .when(!levels.is_empty(), |d| d.child(row("Effort", "⇧ ⌘ E").child(tray)))
            .when(self.current_supports_fast(), |d| {
                d.child(row("Fast", "⌥ ⌘ F").child(Switch::new("model-fast").checked(fast).on_click(move |on, _, cx| {
                    let on = *on;
                    let _ = me_fast.update(cx, |this, cx| if let Some(id) = this.open_session() { this.set_fast(id, on, cx); });
                })))
            })
            .child(
                h_flex().h(theme::z(40.)).px(theme::z(8.)).border_t_1().border_color(p.separator).child(
                    theme::button(&p, "models-edit", "Edit models", Some("settings"), theme::ButtonKind::Ghost).text_color(p.secondary).on_click(move |_, window, cx| {
                        dismiss(&pop_edit, window, cx);
                        let _ = me_edit.update(cx, |this, cx| {
                            this.settings_section = "models";
                            this.open_settings(cx);
                        });
                    }),
                ),
            )
            .into_any_element()
    }

    // ---- the page that edits the loadout ------------------------------------------

    pub(crate) fn models_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let favs = self.favorites();
        let mut col = v_flex();
        if self.models.is_empty() {
            return div().px_4().py_3().set(Type::Callout).text_color(p.secondary).child("Asking the agent which models it has…").into_any_element();
        }
        // the loadout first, in its order; then everything else.
        let mut models: Vec<ModelInfo> = self.models.iter().filter(|m| m.value != "default").cloned().collect();
        models.sort_by_key(|m| favs.iter().position(|f| f.model == m.value).unwrap_or(usize::MAX));
        for m in models {
            let fav = favs.iter().position(|f| f.model == m.value).map(|i| (i, favs[i].clone()));
            let value = m.value.clone();
            let mut row = h_flex()
                .id(SharedString::from(format!("models-row-{}", m.value)))
                .test_support()
                .min_h(px(56.))
                .px_4()
                .py_2()
                .gap_3()
                .border_t_1()
                .border_color(p.separator)
                .when_some(fav.as_ref().map(|(i, _)| *i), |d, i| {
                    let ghost = SharedString::from(m.name.clone());
                    d.on_drag(FavoriteDrag(i), move |_, _, _, cx| cx.new(|_| DragGhost(ghost.clone()))).on_drop(cx.listener(move |this, from: &FavoriteDrag, _, cx| this.move_favorite(from.0, i, cx)))
                })
                .child(div().w(theme::z(14.)).flex_shrink_0().when(fav.is_some(), |d| d.cursor_grab().child(Icon::empty().path("icons/grip-vertical.svg").size(theme::z(13.)).text_color(p.tertiary))))
                .child(model_tile(&m.name, 24., &p))
                .child(v_flex().flex_1().min_w_0().gap(px(2.)).child(div().set(Type::Body).child(m.name.clone())).child(div().set(Type::Subheadline).text_color(p.secondary).truncate().child(m.description.clone())));
            if let Some((_, f)) = &fav {
                // how it runs when picked.
                let mut tray = theme::tray(&p);
                for level in &m.efforts {
                    let (v, l) = (value.clone(), level.clone());
                    tray = tray.child(theme::segment(&p, SharedString::from(format!("models-{}-{level}", m.value)), effort_short(level), f.effort.as_deref() == Some(level.as_str())).test_support().on_click(cx.listener(move |this, _, _, cx| {
                        let l = l.clone();
                        this.edit_favorite(&v, move |f| f.effort = Some(l), cx);
                    })));
                }
                row = row.when(!m.efforts.is_empty(), |d| d.child(tray));
                if m.fast {
                    let v = value.clone();
                    row = row.child(h_flex().gap_2().child(div().set(Type::Callout).text_color(p.secondary).child("Fast")).child(Switch::new(SharedString::from(format!("models-{}-fast", m.value))).checked(f.fast).on_click(cx.listener(move |this, on: &bool, _, cx| {
                        let on = *on;
                        this.edit_favorite(&v, move |f| f.fast = on, cx);
                    }))));
                }
            }
            let starred = fav.is_some();
            let v = value.clone();
            row = row.child(
                theme::button(&p, SharedString::from(format!("models-{}-star", m.value)), "", Some(if starred { "star-fill" } else { "star" }), theme::ButtonKind::Ghost)
                    .text_color(if starred { p.accent } else { p.tertiary })
                    .test_support()
                    .aria_label(SharedString::from(if starred { format!("Remove {} from favourites", m.name) } else { format!("Add {} to favourites", m.name) }))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_favorite(&v, cx))),
            );
            col = col.child(row);
        }
        v_flex()
            .child(div().px_4().py_3().set(Type::Callout).text_color(p.secondary).child(format!(
                "Star up to {MAX_FAVORITES} to keep them in the picker, each with how hard it thinks. Drag to change their order: the first is ⌃⌘1."
            )))
            .child(col)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_model_s_letter_is_its_family() {
        assert_eq!(super::family("Opus 5.5").0, 'O');
        assert_eq!(super::family("claude-haiku-4-5").0, 'H');
        assert_eq!(super::family("Fable 5.2").0, 'F');
        assert_eq!(super::family("Sonnet 5").0, 'S');
        assert_eq!(super::family("gizmo-2").0, 'G');
        assert_eq!(super::family("Opus 4.8").1, super::family("Opus 5.5").1, "one family, one colour");
    }
}
