//! what sits around the message box: files to send with the message, which
//! model answers, how much it may do unasked, and how full its context is.

use super::*;
use base64::Engine as _;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::progress::ProgressCircle;
use tessel_proto::{Attachment, ModelInfo};
use std::path::{Path, PathBuf};

/// permission modes on offer: what the agent calls it, what we call it, what it means.
/// "bypass permissions" is left out on purpose: it is not something to reach by a stray click.
pub(crate) const MODES: [(&str, &str, &str); 4] = [
    ("default", "Ask First", "Asks before editing files or running commands"),
    ("acceptEdits", "Accept Edits", "Edits files freely, asks before running commands"),
    ("auto", "Auto", "Works without asking, and stops itself at anything risky"),
    ("plan", "Plan", "Reads and plans, and changes nothing until you approve"),
];

/// each mode's icon: a lucide path.
pub(crate) fn mode_icon(mode: &str) -> Option<&'static str> {
    Some(match mode {
        "default" => "icons/shield.svg",
        "acceptEdits" => "icons/pencil.svg",
        "auto" => "icons/zap.svg",
        "plan" => "icons/list-checks.svg",
        _ => return None,
    })
}

/// a model id as people say it, for before the agent has named its models:
/// claude-opus-4-7 is Opus 4.7. anything else is left as it is.
pub(crate) fn pretty_model(id: &str) -> Option<String> {
    let rest = id.strip_prefix("claude-")?;
    let mut parts = rest.split('-');
    let family = parts.next().filter(|f| f.chars().all(|c| c.is_ascii_alphabetic()) && !f.is_empty())?;
    // version numbers until a date (20250929) or anything else.
    let version: Vec<&str> = parts.take_while(|p| p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit())).collect();
    if version.is_empty() {
        return None;
    }
    let mut name = family.to_owned();
    name[..1].make_ascii_uppercase();
    Some(format!("{name} {}", version.join(".")))
}

pub(crate) fn mode_name(mode: Option<&str>) -> &'static str {
    match mode {
        None => "Mode",
        Some(m) => MODES.iter().find(|(id, ..)| *id == m).map_or("Custom Mode", |(_, name, _)| name),
    }
}

/// images over this size are refused by the api, so we refuse them first and say why.
const MAX_IMAGE: u64 = 5 * 1024 * 1024;
const MAX_PDF: u64 = 20 * 1024 * 1024;

/// what a file would be sent as. `None` is a kind the agent can't be handed
/// directly; those are mentioned by path instead, and it reads them itself.
pub(crate) fn media_type(path: &Path) -> Option<&'static str> {
    match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        "pdf" => Some("application/pdf"),
        _ => None,
    }
}

/// read a file into something that can go with a message.
pub(crate) fn load_attachment(path: &Path) -> Result<Attachment, String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string_lossy().into_owned());
    let media = media_type(path).ok_or_else(|| format!("{name} is not an image or a PDF."))?;
    let size = std::fs::metadata(path).map_err(|e| format!("Couldn't read {name}: {e}."))?.len();
    let limit = if media == "application/pdf" { MAX_PDF } else { MAX_IMAGE };
    if size > limit {
        return Err(format!("{name} is {} MB. The most that can be attached is {} MB.", size.div_ceil(1024 * 1024), limit / 1024 / 1024));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {name}: {e}."))?;
    Ok(Attachment { name, media_type: media.to_owned(), data: base64::engine::general_purpose::STANDARD.encode(bytes) })
}

/// how full the context is, as a whole percentage. a non-empty context never reads as 0.
pub(crate) fn context_percent(used: u64, window: u64) -> u32 {
    if window == 0 {
        return 0;
    }
    let pct = (used as f64 / window as f64 * 100.0).round() as u32;
    pct.clamp(u32::from(used > 0), 100)
}

/// at this point the agent is close to summarising its own history to make room.
pub(crate) const CONTEXT_WARN: u32 = 80;

impl Shell {
    /// a model's name as the agent presents it, or its id if we haven't been told.
    pub(crate) fn model_name(&self, model: &str) -> String {
        let known = |m: &&ModelInfo| m.value == model || (!m.id.is_empty() && m.id == model);
        // "default" names the same model as another entry; prefer the entry that says which.
        let hit = self.models.iter().filter(|m| m.value != "default").find(known).or_else(|| self.models.iter().find(known));
        hit.map(|m| m.name.clone()).or_else(|| pretty_model(model)).unwrap_or_else(|| model.to_owned())
    }

    pub(crate) fn set_model(&mut self, model: String, cx: &mut Context<Self>) {
        if let Some(session) = self.open_session() {
            self.cmd(Cmd::SetModel { session, model });
        }
        cx.notify();
    }

    pub(crate) fn set_mode(&mut self, mode: String, cx: &mut Context<Self>) {
        if let Some(session) = self.open_session() {
            self.cmd(Cmd::SetMode { session, mode });
        }
        cx.notify();
    }

    pub(crate) fn open_session(&self) -> Option<SessionId> {
        self.model.open.as_ref().map(|o| o.id)
    }

    /// add files to the message being written. images and pdfs go along with
    /// it; anything else is named in the text, for the agent to open itself.
    pub(crate) fn attach_paths(&mut self, paths: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        for path in paths {
            if media_type(&path).is_none() {
                let mention = format!("`{}` ", path.to_string_lossy());
                self.composer.update(cx, |c, cx| c.insert(mention, window, cx));
                continue;
            }
            match load_attachment(&path) {
                Ok(a) => self.attachments.push(a),
                Err(why) => self.error = Some(why),
            }
        }
        self.composer.update(cx, |c, cx| c.focus(window, cx));
        cx.notify();
    }

    /// take what is on the clipboard if it is something to attach. returns
    /// false for plain text, which the message box pastes as usual.
    pub(crate) fn paste(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut took = false;
        for entry in item.entries() {
            match entry {
                ClipboardEntry::Image(image) => {
                    let media = match image.format {
                        ImageFormat::Png => "image/png",
                        ImageFormat::Jpeg => "image/jpeg",
                        ImageFormat::Gif => "image/gif",
                        ImageFormat::Webp => "image/webp",
                        _ => continue,
                    };
                    if image.bytes.len() as u64 > MAX_IMAGE {
                        self.error = Some(format!("That image is {} MB. The most that can be attached is {} MB.", image.bytes.len().div_ceil(1024 * 1024), MAX_IMAGE / 1024 / 1024));
                        took = true;
                        continue;
                    }
                    self.pasted += 1;
                    let ext = media.rsplit('/').next().unwrap_or("png");
                    self.attachments.push(Attachment {
                        name: format!("Pasted image {}.{ext}", self.pasted),
                        media_type: media.to_owned(),
                        data: base64::engine::general_purpose::STANDARD.encode(&image.bytes),
                    });
                    took = true;
                }
                ClipboardEntry::ExternalPaths(paths) => {
                    self.attach_paths(paths.paths().to_vec(), window, cx);
                    took = true;
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if took {
            cx.notify();
        }
        took
    }

    pub(crate) fn choose_attachments(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: true, prompt: Some("Attach".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else { return };
            let _ = this.update_in(cx, |this, window, cx| this.attach_paths(paths, window, cx));
        })
        .detach();
    }

    pub(crate) fn detach(&mut self, ix: usize, cx: &mut Context<Self>) {
        if ix < self.attachments.len() {
            self.attachments.remove(ix);
        }
        cx.notify();
    }

    /// a picture's thumbnail, decoded the first time it's drawn.
    fn thumb(&self, a: &Attachment) -> Option<Arc<Image>> {
        let format = match a.media_type.as_str() {
            "image/png" => ImageFormat::Png,
            "image/jpeg" => ImageFormat::Jpeg,
            "image/gif" => ImageFormat::Gif,
            "image/webp" => ImageFormat::Webp,
            _ => return None,
        };
        let key = (a.data.as_ptr() as usize, a.data.len());
        let mut thumbs = self.thumbs.borrow_mut();
        if let Some(t) = thumbs.get(&key) {
            return Some(t.clone());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(&a.data).ok()?;
        let image = Arc::new(Image::from_bytes(format, bytes));
        thumbs.insert(key, image.clone());
        Some(image)
    }

    /// the files that will go with the next message, each removable: pictures
    /// as themselves, anything else as its name. nothing when there are none.
    pub(crate) fn attachment_chips(&self, cx: &mut Context<Self>) -> Option<Div> {
        // forget the pictures that are no longer going.
        let live: HashSet<(usize, usize)> = self.attachments.iter().map(|a| (a.data.as_ptr() as usize, a.data.len())).collect();
        self.thumbs.borrow_mut().retain(|k, _| live.contains(k));
        if self.attachments.is_empty() {
            return None;
        }
        let p = self.palette.get();
        let mut chips = h_flex().flex_wrap().gap(theme::z(8.));
        for (ix, a) in self.attachments.iter().enumerate() {
            let remove = div()
                .id(("detach", ix))
                .test_support()
                .aria_label(SharedString::from(format!("Remove {}", a.name)))
                .absolute()
                .top(theme::z(-6.))
                .right(theme::z(-6.))
                .size(theme::z(18.))
                .rounded_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(p.raised)
                .border_1()
                .border_color(p.separator)
                .shadow_sm()
                .cursor_pointer()
                .hover(|d| d.bg(p.hover))
                .on_click(cx.listener(move |this, _, _, cx| this.detach(ix, cx)))
                .child(Icon::empty().path("icons/x.svg").size(theme::z(10.)).text_color(p.secondary));
            let chip = match self.thumb(a) {
                Some(image) => div()
                    .size(theme::z(56.))
                    .rounded(theme::z(theme::R_TILE))
                    .border_1()
                    .border_color(p.separator)
                    .overflow_hidden()
                    .child(img(image).size_full().object_fit(ObjectFit::Cover)),
                None => h_flex()
                    .h(theme::z(40.))
                    .pl(theme::z(8.))
                    .pr(theme::z(12.))
                    .gap(theme::z(8.))
                    .rounded(theme::z(theme::R_TILE))
                    .border_1()
                    .border_color(p.separator)
                    .bg(p.fill)
                    .child(
                        div()
                            .size(theme::z(26.))
                            .rounded(theme::z(6.))
                            .bg(p.raised)
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Icon::empty().path("icons/file.svg").size(theme::z(14.)).text_color(p.secondary)),
                    )
                    .child(
                        v_flex()
                            .child(div().max_w(theme::z(180.)).truncate().set(Type::Callout).text_color(p.label).child(a.name.clone()))
                            .child(div().set(Type::Subheadline).text_color(p.tertiary).child(kind_of(&a.media_type, &a.name))),
                    ),
            };
            chips = chips.child(div().relative().child(chip).child(remove));
        }
        Some(chips)
    }

    /// where the message is typed. `bare` leaves off its own border and fill,
    /// for when what is around it is the box.
    pub(crate) fn input_el(&self, bare: bool, cx: &mut Context<Self>) -> AnyElement {
        let paste = cx.entity().downgrade();
        div()
            .id("composer")
            .test_support()
            .aria_label("Message")
            .w_full()
            .child(
                gpui_kit::component::input::Textarea::new(&self.composer)
                    .when(bare, |i| i.appearance(false))
                    .on_paste(move |item, window, cx| paste.update(cx, |this, cx| this.paste(item, window, cx)).unwrap_or(false)),
            )
            .into_any_element()
    }

    /// how much it may do unasked: a popover of the four modes.
    pub(crate) fn mode_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let current = self.model.open_summary().and_then(|s| s.permission_mode.clone());
        let p = self.palette.get();
        let trigger = tool_button("mode-menu").tooltip("How much it may do unasked").child(
            h_flex()
                .gap(theme::z(6.))
                .children(current.as_deref().and_then(mode_icon).map(|icon| Icon::empty().path(icon).size(theme::z(15.)).text_color(p.secondary)))
                .child(div().set(Type::Callout).text_color(p.label).child(mode_name(current.as_deref())))
                .child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(12.)).text_color(p.tertiary)),
        );
        Popover::new("mode-popover").p_0().anchor(Anchor::BottomLeft).trigger(trigger).content(move |_, _, cx| {
            let popover = cx.entity();
            let Some(shell) = me.upgrade() else { return div().into_any_element() };
            let this = shell.read(cx);
            let p = this.palette.get();
            let current = this.model.open_summary().and_then(|s| s.permission_mode.clone());
            let mut list = theme::float_list(&p).w(px(320.));
            for (id, name, what) in MODES {
                let on = current.as_deref() == Some(id);
                let (me, popover) = (me.clone(), popover.clone());
                let icon = mode_icon(id).map(|i| i.trim_start_matches("icons/").trim_end_matches(".svg"));
                list = list.child(
                    theme::float_item(&p, SharedString::from(format!("mode-{id}")), icon, name, Some(what), on)
                        .test_support()
                        .aria_label(SharedString::from(format!("{name}{}", if on { ", current" } else { "" })))
                        .on_click(move |_, window, cx| {
                            let _ = me.update(cx, |this, cx| this.set_mode(id.to_owned(), cx));
                            popover.update(cx, |st, cx| st.dismiss(window, cx));
                        }),
                );
            }
            list.into_any_element()
        })
    }

    /// how full the context is: a ring and a percentage that opens into the
    /// detail. orange once the agent is about to summarise its own history to
    /// make room. before the first turn ends the agent has said how much it
    /// holds but not how much it can, so then it shows the amount alone.
    /// nothing is claimed before anything is known.
    pub(crate) fn context_meter_el(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        let s = self.model.open_summary()?;
        let used = s.context_used?;
        let pct = s.context_window.map(|w| context_percent(used, w));
        let color = if pct.is_some_and(|pct| pct >= CONTEXT_WARN) { p.blocked } else { p.secondary };
        let label: SharedString = match (pct, s.context_window) {
            (Some(pct), Some(window)) => format!("Context {pct}% full, {} of {} tokens", panel::thousands(used), panel::thousands(window)).into(),
            _ => format!("Context: {} tokens so far", panel::thousands(used)).into(),
        };
        let text = match pct {
            Some(pct) => format!("{pct}%"),
            None => format!("{}k", used.div_ceil(1000)),
        };
        let trigger = tool_button("context")
            .child(
                h_flex()
                    .gap(theme::z(6.))
                    .children(pct.map(|pct| ProgressCircle::new("context-ring").size(theme::z(15.)).value(pct as f32).color(color).accessibility_label(label.clone())))
                    .child(div().set(Type::Callout).text_color(p.secondary).child(text)),
            )
            .tooltip(label);
        let me = cx.entity().downgrade();
        Some(
            Popover::new("context-popover")
                .p_0()
                .anchor(Anchor::BottomRight)
                .trigger(trigger)
                .content(move |_, _, cx| {
                    let Some(shell) = me.upgrade() else { return div().into_any_element() };
                    let this = shell.read(cx);
                    let p = this.palette.get();
                    let Some(s) = this.model.open_summary() else { return div().into_any_element() };
                    let used = s.context_used.unwrap_or(0);
                    let row = |k: &str, v: String| h_flex().h(px(26.)).child(div().flex_1().set(Type::Callout).text_color(p.secondary).child(k.to_owned())).child(div().set(Type::Callout).child(v));
                    let mut col = v_flex().w(px(300.)).p_4().gap_1();
                    match s.context_window {
                        Some(window) => {
                            let pct = context_percent(used, window);
                            let color = if pct >= CONTEXT_WARN { p.blocked } else { p.accent };
                            col = col
                                .child(h_flex().items_end().gap_2().child(div().set(Type::Title).child(format!("{pct}%"))).child(div().pb(px(2.)).set(Type::Callout).text_color(p.secondary).child("of the context used")))
                                .child(div().my_2().h(px(6.)).w_full().rounded_full().bg(p.fill).child(div().h_full().rounded_full().bg(color).w(relative((pct as f32 / 100.0).clamp(0.0, 1.0)))))
                                .child(row("Used", format!("{} tokens", panel::thousands(used))))
                                .child(row("Room left", format!("{} tokens", panel::thousands(window.saturating_sub(used)))))
                                .child(row("Window", format!("{} tokens", panel::thousands(window))));
                        }
                        None => col = col.child(row("Used so far", format!("{} tokens", panel::thousands(used)))),
                    }
                    if let Some(m) = &s.model {
                        col = col.child(row("Model", this.model_name(m)));
                    }
                    col = col.child(row("Spent", format!("${:.2}", s.cost_usd)));
                    col.child(div().pt_2().set(Type::Subheadline).text_color(p.secondary).child(format!("At {CONTEXT_WARN}% the agent starts summarising its own history to make room."))).into_any_element()
                })
                .into_any_element(),
        )
    }

    /// what else can go with a message, and how much the agent may do.
    pub(crate) fn plus_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let p_plus = self.palette.get();
        Popover::new("plus-popover")
            .p_0()
            .anchor(Anchor::BottomLeft)
            .trigger(tool_button("composer-plus").w(theme::z(theme::CONTROL_H)).px_0().justify_center().tooltip("Add").child(Icon::empty().path("icons/plus.svg").size(theme::z(16.)).text_color(p_plus.secondary)))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let planning = this.model.open_summary().and_then(|s| s.permission_mode.as_deref()) == Some("plan");
                let pick = |f: fn(&mut Shell, &mut Window, &mut Context<Shell>)| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                theme::float_list(&p)
                    .w(px(248.))
                    .child(theme::float_item_with(&p, "plus-attach", Some("paperclip"), "Add attachment", None, Some("⌘ U"), false).on_click(pick(|this, w, cx| this.choose_attachments(w, cx))))
                    .child(theme::float_item(&p, "plus-plan", Some("list-checks"), if planning { "Leave plan mode" } else { "Plan mode" }, None, planning).on_click(pick(|this, _, cx| {
                        let planning = this.model.open_summary().and_then(|s| s.permission_mode.as_deref()) == Some("plan");
                        this.set_mode(if planning { "default" } else { "plan" }.to_owned(), cx);
                    })))
                    .child(theme::float_item(&p, "plus-link", Some("folder-input"), "Link a folder", Some("Let the agent work there too"), false).on_click(pick(|this, _, cx| this.link_folder(cx))))
                    .into_any_element()
            })
    }

    /// the agent's tool servers, and whether each is working.
    pub(crate) fn mcp_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let servers = self.model.open_summary().map(|s| s.mcp.clone()).unwrap_or_default();
        let broken = servers.iter().filter(|m| matches!(m.status.as_str(), "failed" | "needs-auth")).count();
        let me = cx.entity().downgrade();
        let trigger = tool_button("mcp-menu")
            .when(servers.is_empty(), |b| b.w(theme::z(theme::CONTROL_H)).px_0().justify_center())
            .child(
                h_flex()
                    .gap(theme::z(5.))
                    .child(Icon::empty().path("icons/plug.svg").size(theme::z(15.)).text_color(if broken > 0 { p.blocked } else { p.secondary }))
                    .when(!servers.is_empty(), |d| d.child(div().set(Type::Callout).text_color(p.secondary).child(servers.len().to_string()))),
            )
            .tooltip(if broken > 0 { format!("{broken} tool server{} need{} attention", if broken == 1 { "" } else { "s" }, if broken == 1 { "s" } else { "" }) } else { "Tool Servers".into() });
        Popover::new("mcp-popover").p_0().anchor(Anchor::BottomRight).trigger(trigger).content(move |_, _, cx| {
            let Some(shell) = me.upgrade() else { return div().into_any_element() };
            let this = shell.read(cx);
            let p = this.palette.get();
            let session = this.open_session();
            let servers = this.model.open_summary().map(|s| s.mcp.clone()).unwrap_or_default();
            let refresh = me.clone();
            let mut list = theme::float_list(&p).w(px(300.)).max_h(px(420.));
            list = list.child(
                h_flex()
                    .h(px(32.))
                    .px(px(10.))
                    .child(div().flex_1().set(Type::Callout).font_weight(FontWeight(550.)).child("Tool servers"))
                    .child(Button::new("mcp-refresh").ghost().xsmall().icon(Icon::empty().path("icons/refresh-cw.svg")).tooltip("Check Again").on_click(move |_, _, cx| {
                        if let Some(id) = session {
                            let _ = refresh.update(cx, |this, cx| {
                                this.cmd(Cmd::RefreshMcp(id));
                                cx.notify();
                            });
                        }
                    })),
            );
            if servers.is_empty() {
                list = list.child(div().px(px(10.)).py_2().set(Type::Callout).text_color(p.secondary).child("This session has no tool servers."));
            }
            let mut rows = v_flex().id("mcp-list").overflow_y_scroll().max_h(px(360.));
            for m in &servers {
                let (word, icon, tint) = match m.status.as_str() {
                    "connected" => ("Connected", "check", p.finished),
                    "needs-auth" => ("Needs sign-in", "circle-alert", p.blocked),
                    "failed" => ("Failed", "x", p.failed),
                    "pending" => ("Connecting", "loader", p.secondary),
                    "disabled" => ("Off", "circle-minus", p.tertiary),
                    _ => ("Unknown", "circle-help", p.tertiary),
                };
                let initial = m.name.trim_start_matches("plugin:").chars().next().map(|c| c.to_ascii_uppercase().to_string()).unwrap_or_default();
                rows = rows.child(
                    h_flex()
                        .h(px(32.))
                        .px(px(10.))
                        .gap(px(10.))
                        .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                        .hover(|d| d.bg(p.hover))
                        .child(div().w(px(18.)).flex_shrink_0().set(Type::Callout).text_color(p.tertiary).child(initial))
                        .child(div().flex_1().min_w_0().truncate().set(Type::Body).child(m.name.clone()))
                        .children(m.scope.clone().map(|s| div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(s)))
                        .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).child(word))
                        .child(div().flex_shrink_0().text_color(tint).child(Icon::empty().path(format!("icons/{icon}.svg")).small())),
                );
            }
            list.child(rows).into_any_element()
        })
    }
}

/// a button in the composer's toolbar: as tall as every other control, with
/// room around what it holds.
pub(crate) fn tool_button(id: impl Into<ElementId>) -> Button {
    Button::new(id).ghost().h(theme::z(theme::CONTROL_H)).px(theme::z(8.)).rounded(theme::z(theme::R_CONTROL))
}

#[cfg(test)]
mod tests {
    // named imports: a wildcard would also bring in the ui library's own `test` attribute.
    use super::{CONTEXT_WARN, MAX_IMAGE, context_percent, load_attachment, media_type, mode_name, pretty_model};
    use std::path::Path;

    #[test]
    fn only_what_the_agent_can_be_handed_is_attached() {
        assert_eq!(media_type(Path::new("/tmp/Shot.PNG")), Some("image/png"));
        assert_eq!(media_type(Path::new("a.jpeg")), Some("image/jpeg"));
        assert_eq!(media_type(Path::new("spec.pdf")), Some("application/pdf"));
        assert_eq!(media_type(Path::new("main.rs")), None);
        assert_eq!(media_type(Path::new("Makefile")), None);
    }

    #[test]
    fn a_file_is_read_whole_and_one_too_big_is_refused_with_the_limit() {
        let dir = std::env::temp_dir().join(format!("tessel-attach-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let small = dir.join("dot.png");
        std::fs::write(&small, [1u8, 2, 3]).unwrap();
        let a = load_attachment(&small).unwrap();
        assert_eq!((a.name.as_str(), a.media_type.as_str(), a.data.as_str()), ("dot.png", "image/png", "AQID"));
        assert!(a.is_supported());

        let big = dir.join("huge.png");
        let f = std::fs::File::create(&big).unwrap();
        f.set_len(MAX_IMAGE + 1).unwrap();
        let err = load_attachment(&big).unwrap_err();
        assert!(err.contains("huge.png") && err.contains("5 MB"), "{err}");
        assert!(load_attachment(&dir.join("gone.png")).unwrap_err().contains("gone.png"));
        assert!(load_attachment(&dir.join("notes.txt")).unwrap_err().contains("not an image"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn context_reads_as_a_percentage_and_never_hides_that_something_is_there() {
        assert_eq!(context_percent(0, 1_000_000), 0);
        assert_eq!(context_percent(120, 1_000_000), 1, "a little is not nothing");
        assert_eq!(context_percent(118_204, 1_000_000), 12);
        assert_eq!(context_percent(800_000, 1_000_000), CONTEXT_WARN);
        assert_eq!(context_percent(2_000_000, 1_000_000), 100);
        assert_eq!(context_percent(5, 0), 0, "an unknown window is not a crash");
    }

    #[test]
    fn model_ids_read_the_way_people_say_them() {
        assert_eq!(pretty_model("claude-opus-4-7").as_deref(), Some("Opus 4.7"));
        assert_eq!(pretty_model("claude-haiku-4-5-20251001").as_deref(), Some("Haiku 4.5"));
        assert_eq!(pretty_model("claude-sonnet-5").as_deref(), Some("Sonnet 5"));
        assert_eq!(pretty_model("claude-fable-5-2").as_deref(), Some("Fable 5.2"));
        assert_eq!(pretty_model("something-new"), None);
        assert_eq!(pretty_model("claude-"), None);
        assert_eq!(pretty_model("claude-opus"), None);
    }

    #[test]
    fn modes_have_plain_names() {
        assert_eq!(mode_name(Some("plan")), "Plan");
        assert_eq!(mode_name(Some("default")), "Ask First");
        assert_eq!(mode_name(Some("bypassPermissions")), "Custom Mode");
        assert_eq!(mode_name(None), "Mode");
    }
}

/// what kind of file, in a word: "PDF", "TXT".
fn kind_of(media: &str, name: &str) -> String {
    if media == "application/pdf" {
        return "PDF".into();
    }
    name.rsplit_once('.').map_or_else(|| "File".into(), |(_, ext)| ext.to_uppercase())
}
