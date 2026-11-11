//! how the window looks, chosen whole: themes (tried on by hovering, kept by
//! choosing), layouts, density and material. everything here is a command,
//! so the palette, the settings page, scripts and agents all do it the same way.

use super::*;
use crate::ui::themes::{self, ThemeFile};
use serde_json::{Value, json};

impl Shell {
    /// where the person's own themes live: beside ui.json.
    fn themes_dir(&self) -> Option<std::path::PathBuf> {
        self.ui_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("themes"))
    }

    #[cfg(test)]
    pub(crate) fn theme_preview_for_test(&self) -> Option<String> {
        self.theme_preview.clone()
    }

    pub(crate) fn theme_list(&self) -> Vec<ThemeFile> {
        themes::all(self.themes_dir().as_deref())
    }

    /// try a theme on: the window is drawn with it, nothing is written.
    /// `None` takes it off again.
    pub(crate) fn preview_theme(&mut self, key: Option<&str>, window: &mut Window, cx: &mut Context<Self>) {
        if self.theme_preview.as_deref() == key {
            return;
        }
        let spec = match key.and_then(|k| self.theme_list().into_iter().find(|t| t.key == k)) {
            Some(t) => {
                let mut spec = self.ui.theme.clone();
                spec.base = t.spec.base;
                spec.light = t.spec.light;
                spec.dark_tokens = t.spec.dark_tokens;
                spec
            }
            None => self.ui.theme.clone(),
        };
        self.theme_preview = key.map(str::to_owned);
        self.palette.set(theme::apply(self.forced_dark, &spec, window, cx));
        cx.notify();
    }

    pub(crate) fn use_theme(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let all = self.theme_list();
        let Some(t) = all.iter().find(|t| t.key == key || t.name.eq_ignore_ascii_case(key)) else {
            let names: Vec<&str> = all.iter().map(|t| t.key.as_str()).collect();
            return Err(format!("there's no theme \"{key}\". themes: {}", names.join(", ")));
        };
        let t = t.clone();
        self.theme_preview = None;
        self.edit_ui(|f| {
            let theme = themes::applied(f.get("theme"), &t);
            f.insert("theme".into(), theme);
        }, window, cx);
        Ok(())
    }

    pub(crate) fn use_layout(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let Some((key, ..)) = crate::ui::LAYOUTS.iter().find(|(k, title, _)| *k == name || title.eq_ignore_ascii_case(name)) else {
            let names: Vec<&str> = crate::ui::LAYOUTS.iter().map(|(_, t, _)| *t).collect();
            return Err(format!("there's no layout \"{name}\". layouts: {}", names.join(", ")));
        };
        let key = (*key).to_owned();
        self.edit_ui(|f| {
            f.remove("root");
            if key == "default" {
                f.remove("preset");
            } else {
                f.insert("preset".into(), json!(key));
            }
        }, window, cx);
        Ok(())
    }

    /// which layout preset the window is on, if it's one of them as shipped.
    pub(crate) fn current_layout(&self) -> Option<&'static str> {
        let file = self.ui_path.as_ref().and_then(|p| std::fs::read_to_string(p).ok()).and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or(json!({}));
        if file.get("root").is_some() {
            return None;
        }
        let preset = file.get("preset").and_then(Value::as_str).unwrap_or("default");
        crate::ui::LAYOUTS.iter().find(|(k, ..)| *k == preset).map(|(k, ..)| *k)
    }

    /// a theme as a small picture of a window: frame, sheet, a line of text, the accent.
    pub(crate) fn theme_card(&self, t: &ThemeFile, current: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let dark = p.dark;
        let tp = t.spec.palette(dark);
        let key = t.key.clone();
        let (hover_key, pick_key) = (key.clone(), key.clone());
        v_flex()
            .id(SharedString::from(format!("theme-{key}")))
            .test_support()
            .aria_label(SharedString::from(format!("{}{}", t.name, if current { ", current" } else { "" })))
            .w(theme::z(128.))
            .gap(theme::z(6.))
            .cursor_pointer()
            .on_hover(cx.listener(move |this, hovered: &bool, window, cx| {
                if *hovered {
                    this.preview_theme(Some(&hover_key), window, cx);
                }
            }))
            .on_click(cx.listener(move |this, _, window, cx| {
                if let Err(e) = this.exec("theme.use", &json!({"name": pick_key}), window, cx) {
                    this.error = Some(e);
                }
            }))
            .child(
                h_flex()
                    .h(theme::z(72.))
                    .p(theme::z(6.))
                    .gap(theme::z(5.))
                    .rounded(theme::z(theme::R_CARD))
                    .bg(tp.sidebar)
                    .border_2()
                    .border_color(if current { p.accent } else { p.separator })
                    .child(v_flex().w(theme::z(18.)).gap(theme::z(4.)).pt(theme::z(4.)).children((0..3).map(|i| div().h(theme::z(3.)).w(theme::z(if i == 0 { 16. } else { 12. })).rounded_full().bg(tp.tertiary))))
                    .child(
                        v_flex()
                            .flex_1()
                            .h_full()
                            .p(theme::z(6.))
                            .gap(theme::z(4.))
                            .rounded(theme::z(theme::R_TILE))
                            .bg(tp.window)
                            .child(div().h(theme::z(4.)).w(theme::z(48.)).rounded_full().bg(tp.label))
                            .child(div().h(theme::z(3.)).w(theme::z(36.)).rounded_full().bg(tp.secondary))
                            .child(div().flex_1())
                            .child(h_flex().justify_end().child(div().h(theme::z(10.)).w(theme::z(22.)).rounded_full().bg(tp.accent))),
                    ),
            )
            .child(div().set(Type::Callout).font_weight(if current { FontWeight::MEDIUM } else { FontWeight::NORMAL }).text_color(if current { p.label } else { p.secondary }).child(t.name.clone()))
            .into_any_element()
    }

    /// every theme as a card; hovering tries one on, leaving takes it off.
    pub(crate) fn theme_gallery(&self, cx: &mut Context<Self>) -> AnyElement {
        let list = self.theme_list();
        let mut grid = h_flex().flex_wrap().gap(theme::z(12.));
        for t in &list {
            let current = themes::is_current(t, &self.ui.theme);
            grid = grid.child(self.theme_card(t, current, cx));
        }
        div()
            .id("theme-gallery")
            .p(theme::z(16.))
            .on_hover(cx.listener(|this, hovered: &bool, window, cx| {
                if !*hovered {
                    this.preview_theme(None, window, cx);
                }
            }))
            .child(grid)
            .into_any_element()
    }
}
