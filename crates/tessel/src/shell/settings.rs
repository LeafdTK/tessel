//! settings: the window's layout and look, changed by pointing and clicking.
//!
//! everything here is written to `ui.json`, the same file an agent edits and
//! the window watches. so there is one truth: a setting changed here shows up
//! in the file, and a file edited by hand shows up here.

use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::switch::Switch;
use serde_json::{Map, Value, json};

/// accents to pick from. none of them is a status colour: those mean something.
const ACCENTS: [(&str, &str, Option<&str>); 7] = [
    ("default", "The Theme's Own", None),
    ("blue", "Blue", Some("#0a84ff")),
    ("indigo", "Indigo", Some("#5e5ce6")),
    ("purple", "Purple", Some("#a050dc")),
    ("pink", "Pink", Some("#e5457a")),
    ("teal", "Teal", Some("#1f9eb8")),
    ("graphite", "Graphite", Some("#6e6e73")),
];

/// the pages of settings, in the rail.
pub(crate) const SECTIONS: [(&str, &str, &str); 8] = [
    ("appearance", "Appearance", "icons/sun-moon.svg"),
    ("themes", "Themes", "icons/swatch-book.svg"),
    ("conversation", "Conversation", "icons/message-square.svg"),
    ("models", "Models", "icons/box.svg"),
    ("terminal", "Terminal", "icons/square-terminal.svg"),
    ("agents", "Agents", "icons/bot.svg"),
    ("scripts", "Scripts", "icons/play.svg"),
    ("customize", "Customize", "icons/layout-dashboard.svg"),
];

impl Shell {
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = true;
        self.focus_settings = true;
        self.confirm_reset = false;
        cx.notify();
    }

    pub fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_open = false;
        cx.notify();
    }

    /// what `ui.json` says now, as an object. no file is an empty one.
    pub(crate) fn ui_file(&self) -> Map<String, Value> {
        self.ui_path
            .as_deref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default()
    }

    /// change `ui.json` and show the result at once.
    pub(crate) fn edit_ui(&mut self, change: impl FnOnce(&mut Map<String, Value>), window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.ui_path.clone() else {
            self.error = Some("Settings are saved to ui.json, and this window wasn't started with one.".into());
            cx.notify();
            return;
        };
        let mut file = self.ui_file();
        change(&mut file);
        let text = serde_json::to_string_pretty(&Value::Object(file)).unwrap_or_else(|_| "{}".into()) + "\n";
        if let Err(e) = std::fs::write(&path, text) {
            self.error = Some(format!("Couldn't save {}: {e}.", path.display()));
        }
        self.poll_ui(window, cx);
        cx.notify();
    }

    /// set one key inside an object at the top of the file, making the object if needed.
    pub(crate) fn set_in(file: &mut Map<String, Value>, section: &str, key: &str, value: Option<Value>) {
        let entry = file.entry(section.to_owned()).or_insert_with(|| json!({}));
        // `"theme": "orchard"` is shorthand for `{"base": "orchard"}`.
        if let Value::String(base) = entry {
            *entry = json!({ "base": base.clone() });
        }
        if let Value::Object(obj) = entry {
            match value {
                Some(v) => {
                    obj.insert(key.to_owned(), v);
                }
                None => {
                    obj.remove(key);
                }
            }
            if obj.is_empty() {
                file.remove(section);
            }
        }
    }

    pub(crate) fn set_accent(&mut self, hex: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_ui(
            |f| {
                for appearance in ["light", "dark"] {
                    let theme = f.entry("theme".to_owned()).or_insert_with(|| json!({}));
                    if let Value::String(base) = theme {
                        *theme = json!({ "base": base.clone() });
                    }
                    if let Value::Object(t) = theme {
                        let colours = t.entry(appearance.to_owned()).or_insert_with(|| json!({}));
                        if let Value::Object(c) = colours {
                            match &hex {
                                Some(h) => {
                                    c.insert("accent".into(), json!(h));
                                }
                                None => {
                                    c.remove("accent");
                                }
                            }
                            if c.is_empty() {
                                t.remove(appearance);
                            }
                        }
                        if t.is_empty() {
                            f.remove("theme");
                        }
                    }
                }
            },
            window,
            cx,
        );
    }

    pub(crate) fn apply_fonts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (ui, mono) = (self.font_input.read(cx).value().trim().to_string(), self.mono_input.read(cx).value().trim().to_string());
        self.edit_ui(
            |f| {
                Self::set_in(f, "theme", "font", (!ui.is_empty()).then(|| json!(ui)));
                Self::set_in(f, "theme", "mono", (!mono.is_empty()).then(|| json!(mono)));
            },
            window,
            cx,
        );
    }

    /// open `ui.json` in the text editor, writing the current choice first if there is no file.
    fn open_ui_file(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.ui_path.clone() else { return };
        if !path.exists() {
            let _ = std::fs::write(&path, "{ \"theme\": \"tessel\" }\n");
        }
        #[cfg(target_os = "macos")]
        let _ = std::process::Command::new("open").arg("-t").arg(&path).spawn();
        cx.notify();
    }

    fn reset_ui(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.confirm_reset {
            self.confirm_reset = true;
            cx.notify();
            return;
        }
        self.confirm_reset = false;
        if let Some(path) = self.ui_path.clone() {
            let _ = std::fs::remove_file(path);
        }
        self.poll_ui(window, cx);
        cx.notify();
    }

    // ---- drawing -------------------------------------------------------------

    pub(crate) fn settings_layer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let file = self.ui_file();
        let theme = &self.ui.theme;
        let base = match theme.base.unwrap_or_default() {
            crate::ui::Base::Tessel => "tessel",
            crate::ui::Base::Orchard => "orchard",
        };
        let appearance = match theme.dark.flatten() {
            None => "system",
            Some(false) => "light",
            Some(true) => "dark",
        };
        let size = crate::theme::SCALES.iter().find(|(_, v)| (theme.scale.unwrap_or(1.0) - v).abs() < 0.001).map_or("default", |(n, _)| *n);
        let accent_hex = theme.light.iter().rev().find(|(t, _)| *t == crate::ui::Token::Accent).map(|(_, c)| *c);
        let accent = ACCENTS.iter().find(|(_, _, h)| h.and_then(crate::ui::hex) == accent_hex).map_or("default", |(id, ..)| *id);
        let own_layout = file.contains_key("root");
        let t = self.transcript_opts;

        let _ = base;
        let density = if theme.compact.unwrap_or(false) { "compact" } else { "islands" };
        let material = if theme.glass.unwrap_or(false) {
            "glass"
        } else if theme.felt.unwrap_or(false) {
            "felt"
        } else {
            "solid"
        };
        let backdrop = theme.backdrop.unwrap_or_default().name();
        let themes_rows = v_flex()
            .child(self.picture_row(
                "Material",
                "What the frame around the panes is made of. Text always sits on solid panes.",
                self.pictures("material", &[("solid", "Solid", pic_solid), ("glass", "Glass", pic_glass), ("felt", "Felt", pic_felt)], material, cx, |this, v, w, cx| {
                    let _ = this.exec("theme.material", &json!({"material": v}), w, cx);
                }),
            ))
            .child(self.picture_row(
                "Backdrop",
                "Behind the panes, and filling an empty conversation.",
                self.pictures("backdrop", &[("none", "None", pic_no_backdrop), ("dither", "Dither", pic_dither), ("space", "Space", pic_space)], backdrop, cx, |this, v, w, cx| {
                    let _ = this.exec("theme.backdrop", &json!({"backdrop": v}), w, cx);
                }),
            ))
            .when(backdrop == "space", |d| {
                let st = self.space.borrow();
                let status = if st.loading {
                    "Fetching a photo from NASA's library…".to_owned()
                } else if let Some(e) = &st.error {
                    format!("Couldn't get one: {e}")
                } else if let Some(ph) = &st.photo {
                    if ph.title.is_empty() { "A photo from NASA.".to_owned() } else { format!("Showing \u{201c}{}\u{201d}, from NASA.", ph.title) }
                } else {
                    String::new()
                };
                d.child(
                    h_flex()
                        .px_4()
                        .pb_3()
                        .gap_3()
                        .child(div().flex_1().min_w_0().set(Type::Subheadline).text_color(p.secondary).truncate().child(status))
                        .child(theme::button(&p, "backdrop-next", "Next photo", Some("refresh-cw"), theme::ButtonKind::Secondary).on_click(cx.listener(|this, _, w, cx| {
                            let _ = this.exec("backdrop.next", &json!({}), w, cx);
                        }))),
                )
            })
            .child(self.picture_row(
                "Density",
                "How the panes sit in the window: floating apart, or flush against each other.",
                self.pictures("density", &[("islands", "Islands", pic_islands), ("compact", "Compact", pic_compact)], density, cx, |this, v, w, cx| {
                    let _ = this.exec("theme.density", &json!({"density": v}), w, cx);
                }),
            ))
            .child(
                v_flex()
                    .border_t_1()
                    .border_color(p.separator)
                    .child(
                        v_flex()
                            .px_4()
                            .pt_3()
                            .gap(px(2.))
                            .child(
                                h_flex()
                                    .gap_4()
                                    .child(
                                        v_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap(px(2.))
                                            .child(div().set(Type::Body).child("Theme"))
                                            .child(div().set(Type::Subheadline).text_color(p.secondary).child("Every theme comes light and dark. Hover one to try it on; your own go in ~/.tessel/themes.")),
                                    )
                                    // right by the themes: which of their two looks you see.
                                    .child(div().flex_shrink_0().child(self.segmented("theme-mode", &[("system", "System"), ("light", "Light"), ("dark", "Dark")], appearance, cx, |this, v, w, cx| {
                                        this.edit_ui(|f| Self::set_in(f, "theme", "appearance", (v != "system").then(|| json!(v))), w, cx)
                                    }))),
                            ),
                    )
                    .child(self.theme_gallery(cx)),
            );
        let appearance_rows = v_flex()
            .child(self.setting_row(
                "Appearance",
                "",
                self.segmented("appearance", &[("system", "System"), ("light", "Light"), ("dark", "Dark")], appearance, cx, |this, v, w, cx| {
                    this.edit_ui(|f| Self::set_in(f, "theme", "appearance", (v != "system").then(|| json!(v))), w, cx)
                }),
            ))
            .child(self.setting_row(
                "Interface size",
                "Text, menus and controls.",
                self.segmented("size", &[("compact", "Compact"), ("default", "Default"), ("large", "Large")], size, cx, |this, v, w, cx| {
                    this.edit_ui(|f| Self::set_in(f, "theme", "size", (v != "default").then(|| json!(v))), w, cx)
                }),
            ))
            .child(self.setting_row("Accent", "Selection, focus and the send button.", self.swatches(accent, cx)))
            .child(self.setting_row(
                "Fonts",
                "Interface, then code. Return applies them; empty is the default.",
                h_flex()
                    .gap_2()
                    .child(div().w(px(150.)).child(Input::new(&self.font_input).id("font-input").small()))
                    .child(div().w(px(150.)).child(Input::new(&self.mono_input).id("mono-input").small())),
            ));

        let conversation_rows = v_flex()
            .child(self.setting_row(
                "Your messages",
                "",
                self.segmented("messages", &[("left", "Left"), ("right", "Right")], if t.user_left { "left" } else { "right" }, cx, |this, v, w, cx| {
                    this.edit_ui(|f| Self::set_in(f, "transcript", "user", Some(json!(v))), w, cx)
                }),
            ))
            .child(self.setting_row(
                "Thinking",
                "What the agent thinks before it answers, folded once it's done.",
                self.segmented("thoughts", &[("shown", "Show"), ("hidden", "Hide")], if t.thoughts == Show::Hidden { "hidden" } else { "shown" }, cx, |this, v, w, cx| {
                    this.edit_ui(|f| Self::set_in(f, "transcript", "thoughts", Some(json!(v))), w, cx)
                }),
            ))
            .child(self.setting_row(
                "Tool calls",
                "Anything waiting for you always shows.",
                self.segmented(
                    "tools",
                    &[("shown", "All"), ("failed", "Failures"), ("hidden", "None")],
                    match t.tools {
                        Show::Shown => "shown",
                        Show::Failed => "failed",
                        Show::Hidden => "hidden",
                    },
                    cx,
                    |this, v, w, cx| this.edit_ui(|f| Self::set_in(f, "transcript", "tools", Some(json!(v))), w, cx),
                ),
            ))
            .child(self.setting_row(
                "Activity graphs",
                "How busy each session has been, beside it in the sidebar.",
                {
                    let me = cx.entity().downgrade();
                    Switch::new("activity-graphs").checked(self.sessions_trace).on_click(move |on, window, cx| {
                        let on = *on;
                        let _ = me.update(cx, |this, cx| this.edit_ui(|f| Self::set_in(f, "sessions", "trace", Some(json!(on))), window, cx));
                    })
                },
            ));

        let check = match (&self.ui_error, self.ui.freestyle) {
            (Some(_), _) => ("ui.json has a mistake; the window shows the last layout that loaded.", p.failed),
            (None, true) => ("Freestyle: drawn exactly as written.", p.secondary),
            (None, false) if !self.ui.report.warnings.is_empty() => ("Loaded, with warnings. They're in ui/check.txt.", p.blocked),
            (None, false) => ("Loaded cleanly.", p.secondary),
        };
        let customize_rows = v_flex()
            .child(self.setting_row(
                "From any session",
                "Ask any agent to rearrange this window or draw something in it. Every session has tessel's tools.",
                div(),
            ))
            .child(self.setting_row(
                "ui.json",
                check.0,
                Button::new("settings-open-file").small().outline().label("Open in Editor").on_click(cx.listener(|this, _, _, cx| this.open_ui_file(cx))),
            ))
            .child(self.setting_row("Freestyle", "Turn off the guardrails: no scales, no contrast fixes, no warnings.", {
                let me = cx.entity().downgrade();
                Switch::new("freestyle").checked(self.ui.freestyle).on_click(move |on, window, cx| {
                    let on = *on;
                    let _ = me.update(cx, |this, cx| {
                        this.edit_ui(
                            |f| {
                                if on {
                                    f.insert("freestyle".into(), json!(true));
                                } else {
                                    f.remove("freestyle");
                                }
                            },
                            window,
                            cx,
                        )
                    });
                })
            }))
            .child(self.setting_row(
                "Arrangement",
                "Studio has the workspace beside you. Focus is only the conversation. Tiles adds a terminal and git. Rail puts sessions in a pane.",
                self.segmented(
                    "layout",
                    &[("default", "Studio"), ("focus", "Focus"), ("tiles", "Tiles"), ("rail", "Rail")],
                    self.current_layout().unwrap_or(""),
                    cx,
                    |this, v, w, cx| {
                        if let Err(e) = this.exec("layout.use", &json!({"name": v}), w, cx) {
                            this.error = Some(e);
                        }
                    },
                ),
            ))
            .child(self.setting_row(
                "Layout",
                if own_layout { "Your panes, as you arranged them. Reset puts back the default and keeps your theme." } else { "Split, swap and close panes from their headers. Drag the gaps to resize." },
                Button::new("settings-reset-layout").small().outline().label("Reset Layout").when(!own_layout, |b| b.disabled(true)).on_click(cx.listener(|this, _, window, cx| {
                    this.edit_ui(
                        |f| {
                            f.remove("root");
                            f.remove("preset");
                        },
                        window,
                        cx,
                    )
                })),
            ))
            .child(self.setting_row(
                "Start over",
                if self.confirm_reset { "Deletes ui.json. Click again to go back to the default window." } else { "Back to the default window." },
                Button::new("settings-reset").small().outline().label(if self.confirm_reset { "Delete ui.json" } else { "Reset" }).on_click(cx.listener(|this, _, window, cx| this.reset_ui(window, cx))),
            ));

        let agent_tools = self.daemon_settings.as_ref().map(|d| d.agent_tools);
        let agents_rows = v_flex().child(self.setting_row(
            "Agents can use tessel",
            match agent_tools {
                Some(_) => "Arrange this window and draw live views in the chat. Applies as each agent next starts.",
                None => "The daemon running now is too old to set this. Restart it to get it.",
            },
            {
                let me = cx.entity().downgrade();
                Switch::new("agent-tools").checked(agent_tools.unwrap_or(false)).disabled(agent_tools.is_none()).on_click(move |on, window, cx| {
                    let on = *on;
                    let _ = me.update(cx, |this, cx| {
                        if let Err(e) = this.exec("daemon.set", &json!({"agent_tools": on}), window, cx) {
                            this.error = Some(e);
                        }
                    });
                })
            },
        ));

        let notched = crate::notch::has_notch();
        let notch_about = if notched {
            "The agents at work, around the camera. Hover it to see each one; click one to go to it."
        } else {
            "This screen has no notch, so it's off. Turned on, it hangs from the middle of the menu bar."
        };
        let agents_rows = agents_rows.child(self.setting_row("In the notch", notch_about, {
            let me = cx.entity().downgrade();
            Switch::new("notch").checked(self.ui.notch.unwrap_or(notched)).on_click(move |on, window, cx| {
                let on = *on;
                let _ = me.update(cx, |this, cx| {
                    if let Err(e) = this.exec("app.notch", &json!({"show": on}), window, cx) {
                        this.error = Some(e);
                    }
                });
            })
        }));

        let current = self.settings_section;
        let (title, rows) = match current {
            "conversation" => ("Conversation", conversation_rows),
            "agents" => ("Agents", agents_rows),
            "themes" => ("Themes", themes_rows),
            "models" => ("Models", v_flex().child(self.models_settings(cx))),
            "terminal" => ("Terminal", self.terminal_settings(cx)),
            "scripts" => ("Scripts", self.scripts_settings(cx)),
            "customize" => ("Customize", customize_rows),
            _ => ("Appearance", appearance_rows),
        };
        let mut rail = v_flex().w(px(180.)).flex_shrink_0().p_2().gap(px(2.)).border_r_1().border_color(p.separator);
        for (id, name, icon) in SECTIONS {
            let on = id == current;
            rail = rail.child(
                h_flex()
                    .id(SharedString::from(format!("settings-tab-{id}")))
                    .test_support()
                    .aria_label(SharedString::from(name))
                    .h(px(30.))
                    .px_2()
                    .gap_2()
                    .rounded(px(theme::R_TILE))
                    .set(Type::Body)
                    .when(on, |d| d.bg(p.selection).text_color(p.label))
                    .when(!on, |d| d.text_color(p.secondary).hover(|d| d.bg(p.hover).text_color(p.label)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.settings_section = id;
                        this.confirm_reset = false;
                        cx.notify();
                    }))
                    .child(Icon::empty().path(icon).small())
                    .child(name),
            );
        }

        div()
            .id("settings-layer")
            .absolute()
            .inset_0()
            .occlude()
            .track_focus(&self.settings_focus)
            .key_context("Settings")
            .on_action(cx.listener(|this, _: &crate::CloseSettings, _, cx| this.close_settings(cx)))
            .bg(gpui_kit::black().opacity(if p.dark { 0.45 } else { 0.18 }))
            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .id("settings-page")
                    .test_support()
                    .aria_label("Settings")
                    .w(px(760.))
                    .max_w(relative(0.92))
                    .h(relative(0.8))
                    .max_h(px(600.))
                    .rounded(px(theme::R_PANEL))
                    .border_1()
                    .border_color(p.separator)
                    .bg(p.raised)
                    .shadow_lg()
                    .overflow_hidden()
                    // clicks inside the sheet stay inside it.
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .h(px(52.))
                            .flex_shrink_0()
                            .px_4()
                            .border_b_1()
                            .border_color(p.separator)
                            .child(div().flex_1().set(Type::Title).child("Settings"))
                            .child(Button::new("settings-close").ghost().small().icon(Icon::empty().path("icons/x.svg")).tooltip("Close").on_click(cx.listener(|this, _, _, cx| this.close_settings(cx)))),
                    )
                    .child(
                        h_flex().flex_1().min_h_0().items_start().child(rail.h_full()).child(
                            v_flex()
                                .id("settings-body")
                                .test_support()
                                .flex_1()
                                .h_full()
                                .min_w_0()
                                .overflow_y_scroll()
                                .p_4()
                                .gap_2()
                                // a scrolling column: what's in it keeps its height, or it would
                                // squeeze into the window and clip instead of scrolling.
                                .child(div().flex_shrink_0().px_1().set(Type::Headline).child(title))
                                .child(v_flex().flex_shrink_0().rounded(px(theme::R_CARD)).border_1().border_color(p.separator).bg(p.window).overflow_hidden().child(div().mt(px(-1.)).child(rows))),
                        ),
                    ),
            )
    }

    /// one setting: what it is (and, quietly, what it does) on the left, the control on the right.
    /// the terminal's look: its font (the code font), size and colours.
    fn terminal_settings(&self, cx: &mut Context<Self>) -> Div {
        let t = &self.ui.theme;
        let size = t.term_size.unwrap_or(13.0);
        let size_name = match size as i32 {
            11 => "11",
            12 => "12",
            14 => "14",
            16 => "16",
            _ => "13",
        };
        let colors = t.term_colors.clone().unwrap_or_else(|| "theme".into());
        let colors: &'static str = match colors.as_str() {
            "ink" => "ink",
            "paper" => "paper",
            "midnight" => "midnight",
            _ => "theme",
        };
        v_flex()
            .child(self.setting_row(
                "Font",
                "The code font, shared with code everywhere. Return applies it; empty is the default.",
                div().w(px(180.)).child(Input::new(&self.mono_input).id("terminal-font-input").small()),
            ))
            .child(self.setting_row(
                "Size",
                "In points.",
                self.segmented("terminal-size", &[("11", "11"), ("12", "12"), ("13", "13"), ("14", "14"), ("16", "16")], size_name, cx, |this, v, w, cx| {
                    let _ = this.exec("terminal.look", &json!({"size": v.parse::<f64>().unwrap_or(13.0)}), w, cx);
                }),
            ))
            .child(self.setting_row(
                "Colours",
                "Theme follows the window. The others keep their look whatever the theme.",
                self.segmented("terminal-colors", &[("theme", "Theme"), ("ink", "Ink"), ("paper", "Paper"), ("midnight", "Midnight")], colors, cx, |this, v, w, cx| {
                    let _ = this.exec("terminal.look", &json!({"colors": v}), w, cx);
                }),
            ))
    }

    pub(crate) fn setting_row(&self, label: &'static str, hint: &'static str, control: impl IntoElement) -> Div {
        let p = self.palette.get();
        h_flex()
            .min_h(px(52.))
            .px_4()
            .py_2()
            .gap_4()
            .border_t_1()
            .border_color(p.separator)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(px(2.))
                    .child(div().set(Type::Body).child(label))
                    .when(!hint.is_empty(), |d| d.child(div().set(Type::Subheadline).text_color(p.secondary).child(hint))),
            )
            .child(div().flex_shrink_0().child(control))
    }

    /// a setting whose options are easier seen than said: a row with its name
    /// and hint above, and below it a small picture of each option.
    fn picture_row(&self, label: &'static str, hint: &'static str, pictures: impl IntoElement) -> Div {
        let p = self.palette.get();
        v_flex()
            .px_4()
            .py_3()
            .gap_3()
            .border_t_1()
            .border_color(p.separator)
            .child(
                v_flex()
                    .gap(px(2.))
                    .child(div().set(Type::Body).child(label))
                    .when(!hint.is_empty(), |d| d.child(div().set(Type::Subheadline).text_color(p.secondary).child(hint))),
            )
            .child(pictures)
    }

    /// the options as pictures, the chosen one ringed in the accent.
    fn pictures(
        &self,
        name: &'static str,
        options: &[(&'static str, &'static str, Picture)],
        selected: &str,
        cx: &mut Context<Self>,
        pick: fn(&mut Shell, &'static str, &mut Window, &mut Context<Shell>),
    ) -> impl IntoElement {
        let p = self.palette.get();
        let mut row = h_flex().gap_3().flex_wrap();
        for (value, label, picture) in options.iter().copied() {
            let on = value == selected;
            row = row.child(
                v_flex()
                    .id(SharedString::from(format!("{name}-{value}")))
                    .test_support()
                    .aria_label(SharedString::from(format!("{label}{}", if on { ", selected" } else { "" })))
                    .gap(px(6.))
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| pick(this, value, window, cx)))
                    .child(
                        div()
                            .p(px(3.))
                            .rounded(px(theme::R_CARD + 3.))
                            .border_2()
                            .border_color(if on { p.accent } else { gpui_kit::transparent_black() })
                            .child(div().w(px(128.)).h(px(80.)).rounded(px(theme::R_CARD)).overflow_hidden().border_1().border_color(p.separator).child(picture(&p))),
                    )
                    .child(div().set(Type::Callout).text_color(if on { p.label } else { p.secondary }).child(label)),
            );
        }
        row
    }

    /// one of a few: a sunken tray with the chosen option raised in it.
    pub(crate) fn segmented(
        &self,
        name: &'static str,
        options: &[(&'static str, &'static str)],
        selected: &str,
        cx: &mut Context<Self>,
        pick: fn(&mut Shell, &'static str, &mut Window, &mut Context<Shell>),
    ) -> impl IntoElement {
        let p = self.palette.get();
        let mut tray = theme::tray(&p);
        for (value, label) in options.iter().copied() {
            let on = value == selected;
            tray = tray.child(
                theme::segment(&p, SharedString::from(format!("{name}-{value}")), label, on)
                    .test_support()
                    .aria_label(SharedString::from(format!("{label}{}", if on { ", selected" } else { "" })))
                    .on_click(cx.listener(move |this, _, window, cx| pick(this, value, window, cx))),
            );
        }
        tray
    }

    fn swatches(&self, selected: &str, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let mut row = h_flex().gap_2();
        for (id, name, hex) in ACCENTS {
            let on = id == selected;
            // the theme's own accent, before any accent was picked.
            let own = crate::ui::ThemeSpec { base: self.ui.theme.base, ..Default::default() }.palette(p.dark).accent;
            let color = hex.and_then(crate::ui::hex).unwrap_or(own);
            row = row.child(
                div()
                    .id(SharedString::from(format!("accent-{id}")))
                    .test_support()
                    .aria_label(SharedString::from(format!("{name} accent{}", if on { ", selected" } else { "" })))
                    .size(px(22.))
                    .rounded_full()
                    .p(px(3.))
                    .border_2()
                    .border_color(if on { color } else { gpui_kit::transparent_black() })
                    .on_click(cx.listener(move |this, _, window, cx| this.set_accent(hex.map(str::to_owned), window, cx)))
                    .child(div().size_full().rounded_full().bg(color).when(hex.is_none(), |d| d.border_1().border_color(p.separator))),
            );
        }
        row
    }
}

/// a small drawing of what a setting does.
type Picture = fn(&theme::Palette) -> Div;

/// a little window: a sidebar on the left and two panes, drawn `how`.
fn mini_window(p: &theme::Palette, frame: Hsla, islands: bool, under: Option<Div>, grain: bool) -> Div {
    let pane = |p: &theme::Palette| {
        div().flex_1().h_full().bg(p.window).when(islands, |d| d.rounded(px(4.)).border_1().border_color(p.separator)).child(
            v_flex().p(px(5.)).gap(px(3.)).child(div().w(px(22.)).h(px(3.)).rounded_full().bg(p.tertiary)).child(div().w(px(16.)).h(px(3.)).rounded_full().bg(p.quaternary)),
        )
    };
    let mut body = h_flex().size_full().bg(frame).when(islands, |d| d.p(px(5.)).gap(px(5.))).when(!islands, |d| d.gap(px(1.)));
    body = body
        .child(div().w(px(28.)).h_full().child(v_flex().pt(px(10.)).pl(px(4.)).gap(px(3.)).child(div().w(px(16.)).h(px(3.)).rounded_full().bg(p.tertiary)).child(div().w(px(12.)).h(px(3.)).rounded_full().bg(p.quaternary))))
        .child(pane(p))
        .child(pane(p));
    let mut room = div().relative().size_full();
    if let Some(under) = under {
        room = room.child(under.absolute().inset_0());
    }
    if grain {
        // a scatter of lighter and darker specks: the cloth, small.
        let mut specks = div().absolute().inset_0();
        for i in 0..90u32 {
            let h = i.wrapping_mul(2_654_435_761);
            let (x, y) = ((h % 128) as f32, ((h / 128) % 80) as f32);
            let ink = if h & 1 == 0 { gpui_kit::white().opacity(0.10) } else { gpui_kit::black().opacity(0.18) };
            specks = specks.child(div().absolute().left(px(x)).top(px(y)).size(px(1.5)).bg(ink));
        }
        return room.child(body).child(specks);
    }
    room.child(body)
}

fn frame_of(p: &theme::Palette) -> Hsla {
    p.sidebar.opacity(1.0)
}

fn pic_islands(p: &theme::Palette) -> Div {
    mini_window(p, frame_of(p), true, None, false)
}

fn pic_compact(p: &theme::Palette) -> Div {
    mini_window(p, p.separator, false, None, false)
}

fn pic_solid(p: &theme::Palette) -> Div {
    mini_window(p, frame_of(p), true, None, false)
}

fn pic_glass(p: &theme::Palette) -> Div {
    // the desktop behind, seen through a thin frame.
    let desktop = h_flex().size_full().child(div().flex_1().h_full().bg(gpui_kit::rgb(0x4f7fe3))).child(div().flex_1().h_full().bg(gpui_kit::rgb(0xd9541e))).child(div().flex_1().h_full().bg(gpui_kit::rgb(0x2a9d9a)));
    mini_window(p, frame_of(p).opacity(0.45), true, Some(desktop), false)
}

fn pic_felt(p: &theme::Palette) -> Div {
    mini_window(p, frame_of(p), true, None, true)
}

fn pic_no_backdrop(p: &theme::Palette) -> Div {
    mini_window(p, frame_of(p), true, None, false)
}

fn pic_dither(p: &theme::Palette) -> Div {
    let mut dots = div().size_full().bg(p.window);
    for i in 0..220u32 {
        let h = i.wrapping_mul(2_246_822_519);
        let (x, y) = ((h % 128) as f32, ((h / 128) % 80) as f32);
        let band = ((x + y) - 70.0).abs() < 22.0;
        if band || h % 7 == 0 {
            let c = if (x + y) < 70.0 { gpui_kit::rgb(0x4f7fe3) } else { gpui_kit::rgb(0xd9541e) };
            dots = dots.child(div().absolute().left(px(x)).top(px(y)).size(px(2.)).bg(c));
        }
    }
    dots.relative()
}

fn pic_space(_: &theme::Palette) -> Div {
    // a planet's limb against the dark, as the photos tend to be.
    div()
        .size_full()
        .relative()
        .bg(gpui_kit::rgb(0x05070d))
        .child(div().absolute().left(px(-40.)).top(px(36.)).size(px(150.)).rounded_full().bg(gpui_kit::rgb(0x1d4f8f)).border_2().border_color(gpui_kit::rgb(0x8fc1ec)))
        .child(div().absolute().left(px(96.)).top(px(12.)).size(px(2.)).rounded_full().bg(gpui_kit::white()))
        .child(div().absolute().left(px(70.)).top(px(24.)).size(px(1.5)).rounded_full().bg(gpui_kit::white().opacity(0.7)))
        .child(div().absolute().left(px(114.)).top(px(40.)).size(px(1.5)).rounded_full().bg(gpui_kit::white().opacity(0.6)))
}
