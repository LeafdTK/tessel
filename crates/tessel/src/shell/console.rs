use super::*;
use tessel_proto::Request;
use tessel_proto::task::{Task, TaskAct, TaskState};

const BG: u32 = 0x000000;
const RULE: u32 = 0x262626;
const ZEBRA: u32 = 0x0b0b0b;
const AMBER: u32 = 0xf0a030;
const WHITE: u32 = 0xf2f2f2;
const GREY: u32 = 0x8a8a8a;
const DIM: u32 = 0x5a5a5a;
const GREEN: u32 = 0x3ddc3d;
const RED: u32 = 0xff4545;
const YELLOW: u32 = 0xffd400;
const CYAN: u32 = 0x4fc3ff;
const BAR_RED: u32 = 0xb4002d;
const BAR_ORANGE: u32 = 0xe08000;
const BAR_BLUE: u32 = 0x1d3d8f;
const ROW: f32 = 19.;
const TEXT: f32 = 12.;

fn c(hex: u32) -> Hsla {
    gpui_kit::rgb(hex).into()
}

fn short_age(ms: u64, now: u64) -> String {
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn clock(ms: u64) -> String {
    use chrono::TimeZone as _;
    chrono::Local.timestamp_millis_opt(ms as i64).single().map(|d| d.format("%H:%M:%S").to_string()).unwrap_or_default()
}

fn short_id(id: &str) -> String {
    id.chars().take(6).collect()
}

fn state_word(t: &Task) -> (&'static str, u32) {
    match t.state {
        TaskState::Running if t.building.is_some() => ("OPTS", CYAN),
        TaskState::Running | TaskState::Planned => ("WORK", CYAN),
        TaskState::Proving => ("PROVE", YELLOW),
        TaskState::Landing => ("LAND…", YELLOW),
        TaskState::Checking => ("CI", YELLOW),
        TaskState::Review => ("NEED", RED),
        TaskState::Ready => ("NEXT", GREY),
        TaskState::Draft => ("DRAFT", DIM),
        TaskState::Landed => ("LANDED", GREEN),
        TaskState::Dropped => ("DROP", DIM),
        _ => ("?", DIM),
    }
}

fn bar(title: String, right: String, color: u32, ink: u32) -> Div {
    h_flex()
        .flex_shrink_0()
        .h(px(ROW + 1.))
        .px(px(6.))
        .gap(px(10.))
        .items_center()
        .bg(c(color))
        .text_color(c(ink))
        .font_weight(FontWeight::BOLD)
        .child(div().flex_1().min_w_0().truncate().child(title))
        .child(div().flex_shrink_0().child(right))
}

fn panel() -> Div {
    v_flex().min_w_0().min_h_0().overflow_hidden().bg(c(BG))
}

fn cell(text: impl Into<SharedString>, color: u32, w: Option<f32>) -> Div {
    let d = div().px(px(6.)).truncate().text_color(c(color)).child(text.into());
    match w {
        Some(w) => d.w(px(w)).flex_shrink_0(),
        None => d.flex_1().min_w_0(),
    }
}

fn label(text: &str) -> Div {
    div().pt(px(8.)).text_color(c(GREY)).child(text.to_owned())
}

fn line(text: impl Into<SharedString>, color: u32) -> Div {
    div().text_color(c(color)).child(text.into())
}

fn key(id: String, text: String, color: u32) -> Stateful<Div> {
    div().id(SharedString::from(id)).text_color(c(color)).cursor_pointer().hover(|d| d.underline()).child(text)
}

fn short_tokens(n: u64) -> String {
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

fn task_link(t: &Task) -> Option<(String, String)> {
    match (&t.pr, &t.link, &t.landed) {
        (Some(pr), _, _) => Some((pr.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "PR".to_owned(), |n| format!("PR #{n}")), pr.clone())),
        (None, Some(url), Some(l)) => Some((l.chars().take(7).collect(), url.clone())),
        _ => None,
    }
}

pub(crate) const MODES: [(&str, &str); 4] = [("anything", "me"), ("critical", "agent"), ("ui", "ui"), ("never", "full")];

impl Shell {
    fn task_tokens(&self, t: &Task) -> Option<u64> {
        let s = t.session?;
        let items = if let Some(o) = self.model.open.as_ref().filter(|o| o.id == s) { o.items() } else { self.model.others.get(&s)?.items() };
        let total: u64 = items
            .iter()
            .filter_map(|it| match &it.body {
                tessel_proto::item::ItemBody::Turn { usage: Some(u), .. } => Some(u.input + u.output + u.cache_write),
                _ => None,
            })
            .sum();
        (total > 0).then_some(total)
    }

    pub(crate) fn console_fills(&self) -> bool {
        self.factory_view && self.factory_on()
    }

    pub(crate) fn console_toggle_full(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.console_full = !self.console_full;
        if self.console_full != window.is_fullscreen() {
            window.toggle_fullscreen();
        }
        cx.notify();
    }

    pub(crate) fn console_tasks(&self) -> Vec<&Task> {
        let rank = |s: TaskState| match s {
            TaskState::Review => 0,
            TaskState::Running | TaskState::Planned => 1,
            TaskState::Proving | TaskState::Landing | TaskState::Checking => 2,
            TaskState::Ready => 3,
            TaskState::Draft => 4,
            TaskState::Landed => 5,
            TaskState::Dropped => 6,
            _ => 7,
        };
        let now = super::now_ms();
        let mut v: Vec<&Task> = self
            .tasks
            .iter()
            .filter(|t| !t.cleared && self.board_project.as_ref().is_none_or(|r| &t.repo == r))
            .filter(|t| t.state != TaskState::Dropped || now.saturating_sub(t.updated_ms) < 86_400_000)
            .collect();
        v.sort_by_key(|t| (rank(t.state), std::cmp::Reverse(t.updated_ms)));
        v
    }

    fn console_selected(&self) -> Option<&Task> {
        match self.stage_pick.as_deref() {
            Some(id) => self.tasks.iter().find(|t| t.id == id),
            None => self.console_tasks().into_iter().find(|t| t.state == TaskState::Review),
        }
    }

    pub(crate) fn console(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = super::now_ms();
        if let Some(t) = self.console_selected()
            && self.stage_diff_for.as_deref() != Some(t.id.as_str())
        {
            let me = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = me.update(cx, |this, cx| {
                    this.want_stage_diff();
                    cx.notify();
                });
            });
        }
        let tasks = self.console_tasks();
        let live: Vec<&Task> = tasks.iter().copied().filter(|t| matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking) && t.session.is_some()).take(3).collect();
        let mut left = v_flex().flex_1().min_w_0().h_full().gap(px(2.)).child(div().flex_1().min_h_0().child(self.console_board(&tasks, now, cx)));
        if !live.is_empty() {
            let mut feeds = h_flex().h(px(196.)).flex_shrink_0().gap(px(2.));
            for t in &live {
                feeds = feeds.child(self.console_feed(t, now, 9, cx));
            }
            left = left.child(feeds);
        }
        let right = v_flex()
            .w(relative(0.42))
            .flex_shrink_0()
            .h_full()
            .gap(px(2.))
            .child(div().flex_1().min_h_0().child(self.console_detail(now, cx)))
            .child(self.console_gauges(now))
            .child(div().h(px(172.)).flex_shrink_0().child(self.console_events(now)));
        v_flex()
            .id("console")
            .size_full()
            .min_w_0()
            .bg(c(BG))
            .p(px(2.))
            .gap(px(2.))
            .font_family(theme::mono())
            .text_size(px(TEXT))
            .line_height(px(ROW - 3.))
            .text_color(c(AMBER))
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.console_key(ev, window, cx)))
            .child(self.console_command(cx))
            .child(h_flex().flex_1().min_h_0().gap(px(2.)).child(left).child(right))
            .test_support()
            .into_any_element()
    }

    fn console_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let k = &ev.keystroke;
        if k.key == "f" && k.modifiers.platform && k.modifiers.control {
            self.console_toggle_full(window, cx);
            cx.stop_propagation();
            return;
        }
        if k.key == "escape" {
            if self.console_full {
                self.console_toggle_full(window, cx);
            } else {
                self.stage_pick = None;
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if (k.key == "up" || k.key == "down") && !k.modifiers.platform {
            let ids: Vec<String> = self.console_tasks().iter().map(|t| t.id.clone()).collect();
            if ids.is_empty() {
                return;
            }
            let at = self.console_selected().and_then(|t| ids.iter().position(|i| *i == t.id));
            let next = match (at, k.key.as_str()) {
                (None, "down") => 0,
                (None, _) => ids.len() - 1,
                (Some(i), "down") => (i + 1).min(ids.len() - 1),
                (Some(i), _) => i.saturating_sub(1),
            };
            self.stage_pick = Some(ids[next].clone());
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn console_command(&self, cx: &mut Context<Self>) -> AnyElement {
        let paused = self.factory_paused();
        let host = self.hosts.list.current().map_or_else(|| "this mac".to_owned(), |h| h.name.clone());
        let project = self.board_project.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "all projects".into());
        let mode = self.daemon_settings.as_ref().map_or_else(|| "agent".to_owned(), |d| d.autonomy.clone());
        let mode = MODES.iter().find(|(_, v)| *v == mode).map_or("own rule", |(w, _)| w).to_owned();
        let full = self.console_full;
        let button = |id: &'static str, text: String| {
            div()
                .id(id)
                .px(px(8.))
                .h(px(20.))
                .flex()
                .items_center()
                .border_1()
                .border_color(c(0x444444))
                .text_color(c(WHITE))
                .cursor_pointer()
                .hover(|d| d.bg(c(0x2a2a2a)))
                .child(text)
        };
        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .pl(px(if full { 6. } else { 78. }))
            .pr(px(6.))
            .gap(px(10.))
            .items_center()
            .bg(c(0x1a1a1a))
            .child(div().text_color(c(YELLOW)).font_weight(FontWeight::BOLD).child("TESSEL"))
            .child(div().text_color(c(WHITE)).child(host))
            .child(div().text_color(c(GREY)).child(project))
            .child(div().text_color(c(GREY)).child(format!("bother: {mode}")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h(px(20.))
                    .px(px(6.))
                    .flex()
                    .items_center()
                    .bg(c(BG))
                    .border_1()
                    .border_color(c(0x444444))
                    .text_color(c(0xffbf3f))
                    .child(div().w_full().child(Input::new(&self.console_input).id("console-input").appearance(false))),
            )
            .children(self.console_said.clone().filter(|(_, s)| !s.is_empty()).map(|(ok, said)| div().max_w(px(360.)).truncate().text_color(c(if ok { GREEN } else { RED })).child(said)))
            .child(
                div()
                    .id("console-go")
                    .px(px(10.))
                    .h(px(20.))
                    .flex()
                    .items_center()
                    .bg(c(0x1f6f1f))
                    .text_color(c(WHITE))
                    .font_weight(FontWeight::BOLD)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| this.console_run(window, cx)))
                    .test_support()
                    .child("GO"),
            )
            .child(div().text_color(c(if paused { RED } else { GREEN })).child(if paused { "● PAUSED" } else { "● RUNNING" }))
            .child(
                button("console-chats", "CHATS".into())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let _ = this.exec("app.view", &serde_json::json!({"view": "chats"}), window, cx);
                    }))
                    .test_support(),
            )
            .child(button("console-full", if full { "EXIT ⌃⌘F".into() } else { "FULL ⌃⌘F".into() }).on_click(cx.listener(|this, _, window, cx| this.console_toggle_full(window, cx))).test_support())
            .into_any_element()
    }

    fn console_board(&self, tasks: &[&Task], now: u64, cx: &mut Context<Self>) -> AnyElement {
        let count = |f: &dyn Fn(&Task) -> bool| tasks.iter().filter(|t| f(t)).count();
        let summary = [
            (count(&|t| t.state == TaskState::Review), "NEED"),
            (count(&|t| matches!(t.state, TaskState::Running | TaskState::Planned)), "WORK"),
            (count(&|t| matches!(t.state, TaskState::Proving | TaskState::Landing | TaskState::Checking)), "CHECK"),
            (count(&|t| t.state == TaskState::Ready), "NEXT"),
            (count(&|t| t.state == TaskState::Draft), "DRAFT"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, w)| format!("{n} {w}"))
        .collect::<Vec<_>>()
        .join("  ");
        let selected = self.console_selected().map(|t| t.id.clone());
        let head = h_flex()
            .h(px(ROW))
            .flex_shrink_0()
            .border_b_1()
            .border_color(c(RULE))
            .child(cell("ID", GREY, Some(64.)))
            .child(cell("TASK", GREY, None))
            .child(cell("ST", GREY, Some(64.)))
            .child(cell("AGE", GREY, Some(44.)))
            .child(cell("NOW", GREY, None))
            .child(cell("TOKENS", GREY, Some(70.)))
            .child(cell("DIFF", GREY, Some(100.)))
            .child(cell("LINK", GREY, Some(96.)));
        let mut rows = v_flex().id("console-board-rows").flex_1().min_h_0().overflow_y_scroll();
        for (i, t) in tasks.iter().enumerate() {
            let (word, tone) = state_word(t);
            let now_text = match t.state {
                TaskState::Review => t.ask.clone().unwrap_or_else(|| self.outcome(t)),
                TaskState::Landed | TaskState::Dropped => self.outcome(t),
                TaskState::Ready if !t.deps.is_empty() => format!("after {}", t.deps.iter().map(|d| short_id(d)).collect::<Vec<_>>().join(", ")),
                TaskState::Draft => String::new(),
                _ => self.stage_doing(t),
            };
            let tokens = self.task_tokens(t);
            let (added, removed) = t.changes.iter().fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
            let on = selected.as_deref() == Some(t.id.as_str());
            let id = t.id.clone();
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("console-row-{}", t.id)))
                    .h(px(ROW))
                    .flex_shrink_0()
                    .items_center()
                    .when(i % 2 == 1 && !on, |d| d.bg(c(ZEBRA)))
                    .when(on, |d| d.bg(c(0x3a2a00)))
                    .hover(|d| d.bg(c(0x1a1a1a)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stage_pick = Some(id.clone());
                        cx.notify();
                    }))
                    .test_support()
                    .child(cell(short_id(&t.id), WHITE, Some(64.)))
                    .child(cell(t.title.clone(), WHITE, None))
                    .child(cell(word, tone, Some(64.)))
                    .child(cell(short_age(t.created_ms, now), AMBER, Some(44.)))
                    .child(cell(now_text, GREY, None))
                    .child(cell(tokens.map_or("—".into(), short_tokens), AMBER, Some(70.)))
                    .child(
                        h_flex()
                            .w(px(100.))
                            .flex_shrink_0()
                            .px(px(6.))
                            .gap(px(6.))
                            .when(added + removed > 0, |d| d.child(div().text_color(c(GREEN)).child(format!("+{added}"))).child(div().text_color(c(RED)).child(format!("−{removed}"))))
                            .when(added + removed == 0, |d| d.child(div().text_color(c(DIM)).child("—"))),
                    )
                    .child(match task_link(t) {
                        Some((text, url)) => key(format!("console-link-{}", t.id), text, CYAN)
                            .w(px(96.))
                            .flex_shrink_0()
                            .px(px(6.))
                            .truncate()
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                cx.open_url(&url);
                            })
                            .into_any_element(),
                        None => cell("", DIM, Some(96.)).into_any_element(),
                    }),
            );
        }
        if tasks.is_empty() {
            rows = rows.child(div().p(px(8.)).text_color(c(GREY)).child("No tasks. Type:  new <what to do>   slack <channel>   help"));
        }
        panel().size_full().child(bar("BOARD".into(), summary, BAR_RED, WHITE)).child(head).child(rows).into_any_element()
    }

    fn console_gauges(&self, now: u64) -> AnyElement {
        let day = 86_400_000u64;
        let today_start = {
            use chrono::TimeZone as _;
            chrono::Local::now().date_naive().and_hms_opt(0, 0, 0).and_then(|d| chrono::Local.from_local_datetime(&d).single()).map_or(now.saturating_sub(day), |d| d.timestamp_millis() as u64)
        };
        let mine = |t: &&Task| self.board_project.as_ref().is_none_or(|r| &t.repo == r);
        let landed_today: Vec<&Task> = self.tasks.iter().filter(mine).filter(|t| t.state == TaskState::Landed && t.updated_ms >= today_start).collect();
        let known: Vec<u64> = landed_today.iter().filter_map(|t| self.task_tokens(t)).collect();
        let per = (!known.is_empty()).then(|| known.iter().sum::<u64>() as f64 / known.len() as f64);
        let recent: Vec<&Task> = self.tasks.iter().filter(mine).filter(|t| now.saturating_sub(t.updated_ms) < day && matches!(t.state, TaskState::Landed | TaskState::Dropped)).collect();
        let success = (!recent.is_empty()).then(|| recent.iter().filter(|t| t.state == TaskState::Landed).count() * 100 / recent.len());
        let limit = |name: &str| self.usage.iter().find(|w| w.name == name).map(|w| if w.resets_at.is_some_and(|r| (r as u64) * 1000 <= now) { 0 } else { (w.used * 100.0).round() as u32 });
        let tone = |v: Option<u32>| match v {
            Some(v) if v >= 90 => RED,
            Some(v) if v >= 70 => YELLOW,
            _ => WHITE,
        };
        let pct = |v: Option<u32>| v.map_or("—".to_owned(), |v| format!("{v}%"));
        let tile = |label: &str, value: String, color: u32| v_flex().flex_1().min_w_0().px(px(6.)).py(px(4.)).bg(c(ZEBRA)).child(div().text_color(c(GREY)).truncate().child(label.to_owned())).child(div().text_size(px(20.)).line_height(px(24.)).text_color(c(color)).child(value));
        let (five, week) = (limit("five_hour"), limit("seven_day"));
        panel()
            .flex_shrink_0()
            .child(bar("TODAY".into(), String::new(), BAR_ORANGE, 0x000000))
            .child(
                h_flex()
                    .gap(px(2.))
                    .p(px(2.))
                    .child(tile("LANDED", landed_today.len().to_string(), WHITE))
                    .child(tile("TOKENS/LANDED", per.map_or("—".into(), |v| short_tokens(v as u64)), WHITE))
                    .child(tile("SUCCESS", success.map_or("—".into(), |s| format!("{s}%")), WHITE))
                    .child(tile("5H", pct(five), tone(five)))
                    .child(tile("WEEK", pct(week), tone(week))),
            )
            .into_any_element()
    }

    fn console_feed(&self, t: &Task, now: u64, most: usize, cx: &mut Context<Self>) -> AnyElement {
        let (word, _) = state_word(t);
        let id = t.id.clone();
        panel()
            .id(SharedString::from(format!("console-feed-{}", t.id)))
            .flex_1()
            .h_full()
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.stage_pick = Some(id.clone());
                cx.notify();
            }))
            .child(bar(format!("{} {}  {}", short_id(&t.id), word, t.title), short_age(t.updated_ms.min(now), now), BAR_BLUE, WHITE))
            .child(self.console_steps(t, most))
            .into_any_element()
    }

    fn console_steps(&self, t: &Task, most: usize) -> Div {
        let steps = t.session.map(|s| self.steps(s, most)).unwrap_or_default();
        let last = steps.len().saturating_sub(1);
        let mut col = v_flex().flex_1().min_h_0().justify_end().px(px(6.)).py(px(2.));
        for (i, s) in steps.into_iter().enumerate() {
            let verb = if s.verb.is_empty() { "SAY".to_owned() } else { s.verb.to_uppercase().chars().take(6).collect() };
            col = col.child(
                h_flex()
                    .h(px(ROW - 2.))
                    .flex_shrink_0()
                    .gap(px(8.))
                    .child(div().w(px(48.)).flex_shrink_0().text_color(c(if i == last { GREEN } else { GREY })).child(verb))
                    .child(div().flex_1().min_w_0().truncate().text_color(c(if s.failed { RED } else if i == last { WHITE } else { AMBER })).child(s.what)),
            );
        }
        col
    }

    fn console_detail(&self, now: u64, cx: &mut Context<Self>) -> AnyElement {
        match self.stage_pick.as_deref() {
            Some("new") => return self.console_new(cx),
            Some("issues") => return self.console_issues(cx),
            Some("rules") => return self.console_rules(),
            Some("help") => return self.console_help(),
            _ => {}
        }
        match self.console_selected() {
            Some(t) => self.console_task(t, now, cx),
            None => self.console_help(),
        }
    }

    fn console_task(&self, t: &Task, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let (word, tone) = state_word(t);
        let id = t.id.clone();
        let mut links = h_flex().gap(px(14.));
        for (name, url) in [("PR", t.pr.clone()), ("COMMIT", t.link.clone()), ("ISSUE", t.issue.clone()), ("SLACK", t.source.clone())] {
            if let Some(url) = url {
                links = links.child(key(format!("console-{name}-{id}"), format!("[{name}]"), CYAN).on_click(move |_, _, cx| cx.open_url(&url)));
            }
        }
        if let Some(s) = t.session {
            links = links.child(key(format!("console-chat-{id}"), "[CHAT]".into(), CYAN).on_click(cx.listener(move |this, _, _, cx| {
                this.factory_view = false;
                this.switch_open(s, false, cx);
                cx.notify();
            })));
        }
        let mut body = v_flex().id("console-detail-body").flex_1().min_h_0().overflow_y_scroll().px(px(8.)).py(px(6.)).gap(px(2.));
        body = body.child(line(t.title.clone(), WHITE));
        body = body.child(h_flex().gap(px(14.)).child(line(word, tone)).child(line(format!("age {}", short_age(t.created_ms, now)), GREY)).child(line(self.task_tokens(t).map_or_else(String::new, |n| format!("{} tokens", short_tokens(n))), AMBER)).child(links));
        if t.state == TaskState::Review {
            body = body.child(label("QUESTION")).child(line(t.ask.clone().unwrap_or_else(|| self.outcome(t)), WHITE));
            let mut keys = v_flex();
            for (n, o) in t.options.iter().enumerate() {
                let i = id.clone();
                let text = format!("[{}] {}{}", n + 1, o.label, if o.what.is_empty() { String::new() } else { format!(" — {}", o.what) });
                keys = keys.child(key(format!("console-choose-{id}-{n}"), text, if n == 0 { GREEN } else { CYAN }).on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Choose { index: n }, cx))));
            }
            if t.options.is_empty() && (!t.changes.is_empty() || !t.shots.is_empty()) {
                let i = id.clone();
                keys = keys.child(key(format!("console-land-{id}"), "[L] land".into(), GREEN).on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))));
            }
            keys = keys.child(line("[T] t <what to change>", GREY));
            body = body.child(keys);
            let pics: Vec<(usize, String)> = t.options.iter().enumerate().filter_map(|(n, o)| o.shots.first().map(|s| (n, s.clone()))).collect();
            if !pics.is_empty() {
                let mut row = h_flex().gap(px(6.)).pt(px(4.));
                for (n, path) in pics {
                    row = row.child(v_flex().w(px(150.)).child(div().w_full().h(px(92.)).bg(c(ZEBRA)).overflow_hidden().children(self.shot_src(&path).map(|s| img(s).size_full().object_fit(ObjectFit::Cover)))).child(line(format!("[{}]", n + 1), GREY)));
                }
                body = body.child(row);
            }
        }
        if matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking) {
            body = body.child(label("LIVE")).child(self.console_steps(t, 14));
        }
        if !t.shots.is_empty() {
            let mut row = h_flex().gap(px(6.)).flex_wrap();
            for (n, path) in t.shots.iter().take(6).enumerate() {
                let open = path.clone();
                row = row.child(
                    div()
                        .id(SharedString::from(format!("console-shot-{n}")))
                        .w(px(150.))
                        .h(px(92.))
                        .bg(c(ZEBRA))
                        .overflow_hidden()
                        .cursor_pointer()
                        .on_click(move |_, _, cx| cx.open_with_system(std::path::Path::new(&open)))
                        .children(self.shot_src(path).map(|s| img(s).size_full().object_fit(ObjectFit::Cover))),
                );
            }
            body = body.child(label("SCREENSHOTS")).child(row);
        }
        if !t.changes.is_empty() {
            body = body.child(label("CHANGES"));
            for f in t.changes.iter().take(20) {
                body = body.child(h_flex().gap(px(8.)).child(div().flex_1().min_w_0().truncate().text_color(c(WHITE)).child(f.path.clone())).child(div().text_color(c(GREEN)).child(format!("+{}", f.added))).child(div().text_color(c(RED)).child(format!("−{}", f.removed))));
            }
            if self.stage_diff_for.as_deref() == Some(t.id.as_str())
                && let Some(diff) = self.stage_diff_el(&self.palette.get())
            {
                body = body.child(div().pt(px(4.)).child(diff));
            }
        }
        if let Some(proof) = t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).and_then(|s| s.proof.clone()) {
            body = body.child(label("CHECKS"));
            for v in &proof.verify {
                body = body.child(line(format!("{} {}", if v.ok { "PASS" } else { "FAIL" }, v.command), if v.ok { GREEN } else { RED }));
            }
            for tr in &proof.tried {
                body = body.child(line(format!("{} {}", if tr.ok { "PASS" } else { "FAIL" }, tr.title), if tr.ok { GREEN } else { RED }));
            }
            for f in proof.standing().filter(|f| f.severity != tessel_proto::proof::Severity::Note).take(6) {
                body = body.child(line(format!("FIND {}", f.title), YELLOW));
            }
        }
        if let Some(r) = &t.review {
            body = body.child(label("REVIEWER")).child(line(r.clone(), WHITE));
        }
        if !t.spec.trim().is_empty() {
            body = body.child(label("SPEC"));
            for l in t.spec.lines().take(14) {
                body = body.child(line(l.to_owned(), AMBER));
            }
        }
        if !t.notes.is_empty() {
            body = body.child(label("HISTORY"));
            for n in t.notes.iter().rev().take(20) {
                body = body.child(h_flex().gap(px(8.)).child(div().flex_shrink_0().text_color(c(DIM)).child(clock(n.at_ms))).child(div().flex_1().min_w_0().text_color(c(if n.by == "you" { WHITE } else { AMBER })).child(super::sentence(&n.text))));
            }
        }
        panel().size_full().child(bar(format!("{}  {}", short_id(&t.id), word), "↑↓ select · esc back".into(), BAR_BLUE, WHITE)).child(body).into_any_element()
    }

    fn console_new(&self, cx: &mut Context<Self>) -> AnyElement {
        let repo = self.board_repo().map(|r| super::projects::folder_name(&r)).unwrap_or_else(|| "no project: type  project <name>".into());
        let dest = self.board_land.clone().unwrap_or_else(|| self.daemon_settings.as_ref().map_or_else(|| "main".into(), |d| d.land.clone()));
        let attached: Vec<String> = self.board_attachments.iter().map(|a| a.name.clone()).chain(self.board_files.iter().map(|f| f.rsplit('/').next().unwrap_or(f).to_owned())).collect();
        let me = cx.entity().downgrade();
        panel()
            .size_full()
            .child(bar("NEW TASK".into(), format!("{repo} → {dest}"), BAR_BLUE, WHITE))
            .child(
                v_flex()
                    .flex_1()
                    .min_h_0()
                    .p(px(8.))
                    .gap(px(8.))
                    .child(
                        div()
                            .id("console-new-box")
                            .flex_1()
                            .min_h(px(120.))
                            .p(px(6.))
                            .border_1()
                            .border_color(c(0x444444))
                            .text_color(c(WHITE))
                            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.board_attach_paths(paths.paths().to_vec(), cx)))
                            .child(gpui_kit::component::input::Textarea::new(&self.task_title).appearance(false).on_paste(move |item, window, cx| me.update(cx, |this, cx| this.board_paste(item, window, cx)).unwrap_or(false))),
                    )
                    .when(!attached.is_empty(), |d| d.child(line(format!("attached: {}", attached.join(", ")), GREY)))
                    .child(line("A list makes one task per line. A long spec gets planned first. #42 brings in that issue.", DIM))
                    .child(
                        h_flex()
                            .gap(px(14.))
                            .child(key("console-new-start".into(), "[START]".into(), GREEN).on_click(cx.listener(|this, _, window, cx| this.add_task(true, window, cx))))
                            .child(key("console-new-draft".into(), "[DRAFT]".into(), CYAN).on_click(cx.listener(|this, _, window, cx| this.add_task(false, window, cx))))
                            .child(line("dest main|pr|keep|branch <name> changes where it lands", DIM)),
                    ),
            )
            .into_any_element()
    }

    fn console_issues(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = v_flex().id("console-issues").flex_1().min_h_0().overflow_y_scroll().p(px(6.));
        match &self.board_issues {
            None => body = body.child(line("Pick a project: project <name>", GREY)),
            Some((_, Err(e))) => body = body.child(line(e.clone(), RED)),
            Some((_, Ok(list))) if list.is_empty() => body = body.child(line(if self.issues_loading { "Asking GitHub…" } else { "No open issues." }, GREY)),
            Some((repo, Ok(list))) => {
                for (i, issue) in list.iter().enumerate() {
                    let filed = self.tasks.iter().any(|t| t.issue.as_deref() == Some(issue.url.as_str()) && t.state != TaskState::Dropped);
                    let (r, url) = (repo.clone(), issue.url.clone());
                    body = body.child(
                        h_flex()
                            .h(px(ROW))
                            .gap(px(8.))
                            .when(i % 2 == 1, |d| d.bg(c(ZEBRA)))
                            .child(cell(format!("#{}", issue.number), WHITE, Some(56.)))
                            .child(cell(issue.title.clone(), AMBER, None))
                            .child(if filed {
                                line("queued", DIM).into_any_element()
                            } else {
                                key(format!("console-solve-{}", issue.number), "[START]".into(), GREEN)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.cmd(Cmd::Ask { tag: "task", req: Request::TaskFromIssue { repo: r.clone(), issue: url.clone(), ready: true } });
                                        cx.notify();
                                    }))
                                    .into_any_element()
                            }),
                    );
                }
            }
        }
        panel().size_full().child(bar("ISSUES".into(), "solve <number>".into(), BAR_BLUE, WHITE)).child(body).into_any_element()
    }

    fn console_rules(&self) -> AnyElement {
        let mut body = v_flex().id("console-rules").flex_1().min_h_0().overflow_y_scroll().p(px(6.));
        match &self.board_rules {
            None => body = body.child(line("Pick a project: project <name>", GREY)),
            Some((_, None)) => body = body.child(line("Reading them…", GREY)),
            Some((_, Some(Err(e)))) => body = body.child(line(e.clone(), RED)),
            Some((_, Some(Ok(book)))) => {
                body = body.child(label("RULES"));
                for r in &book.rules {
                    body = body.child(h_flex().gap(px(8.)).child(cell(r.id.clone(), WHITE, Some(180.))).child(cell(r.title.clone(), AMBER, None)));
                }
                if !book.knowledge.is_empty() {
                    body = body.child(label("KNOWLEDGE"));
                    for k in &book.knowledge {
                        body = body.child(h_flex().gap(px(8.)).child(cell(k.id.clone(), WHITE, Some(180.))).child(cell(k.title.clone(), AMBER, None)));
                    }
                }
                if !book.verify.is_empty() {
                    body = body.child(label("VERIFY"));
                    for v in &book.verify {
                        body = body.child(line(v.clone(), GREY));
                    }
                }
            }
        }
        panel().size_full().child(bar("RULES".into(), "teach <how we do things here>".into(), BAR_BLUE, WHITE)).child(body).into_any_element()
    }

    fn console_help(&self) -> AnyElement {
        let hosts: Vec<String> = std::iter::once("mac".to_owned()).chain(self.hosts.list.hosts.iter().map(|h| h.name.clone())).collect();
        let projects: Vec<String> = self.board_projects().iter().map(|p| super::projects::folder_name(p)).collect();
        let rows: [(&str, &str); 22] = [
            ("new <text>", "start a task (a list makes several; no text opens the editor)"),
            ("add <text>", "save a draft"),
            ("↑ ↓", "select a task; its detail shows here"),
            ("l / d / s", "land / drop / start the selected task"),
            ("1 2 3", "answer the selected question with that option"),
            ("t <text>", "tell the selected task what to change"),
            ("o", "open its chat"),
            ("land|drop|start|plan|clear <id>", "act on any task by id prefix"),
            ("slack <channel> [since mon] [note]", "read a channel into drafts"),
            ("checkup", "look through the project for things to fix"),
            ("issues / solve <n>", "github issues"),
            ("rules / teach <text>", "house rules"),
            ("project <name>|all", "which project"),
            ("host <name>|mac", "which machine"),
            ("mode anything|critical|ui|never", "when to bother you"),
            ("mode rule <text>", "your own rule"),
            ("dest main|pr|keep|branch <name>", "where new tasks land"),
            ("pause / resume", "the whole factory"),
            ("clear done", "take landed work off the board"),
            ("chats", "back to chats"),
            ("full", "fullscreen (⌃⌘F, esc leaves)"),
            ("help", "this"),
        ];
        let mut body = v_flex().id("console-help").flex_1().min_h_0().overflow_y_scroll().p(px(6.));
        for (cmd, what) in rows {
            body = body.child(h_flex().gap(px(10.)).child(div().w(px(250.)).flex_shrink_0().text_color(c(WHITE)).child(cmd)).child(div().flex_1().min_w_0().text_color(c(AMBER)).child(what)));
        }
        body = body.child(label("HOSTS")).child(line(hosts.join("  "), WHITE));
        if !projects.is_empty() {
            body = body.child(label("PROJECTS")).child(line(projects.join("  "), WHITE));
        }
        panel().size_full().child(bar("HELP".into(), String::new(), BAR_BLUE, WHITE)).child(body).into_any_element()
    }

    fn console_events(&self, now: u64) -> AnyElement {
        let mut events: Vec<(u64, String, String)> = Vec::new();
        let mine = |id: &str| self.board_project.as_ref().is_none_or(|r| self.tasks.iter().any(|t| t.id == id && &t.repo == r));
        if self.task_events.is_empty() {
            for t in self.tasks.iter().filter(|t| self.board_project.as_ref().is_none_or(|r| &t.repo == r)) {
                for n in &t.notes {
                    events.push((n.at_ms, short_id(&t.id), super::sentence(&n.text)));
                }
            }
        } else {
            for e in self.task_events.iter().filter(|e| mine(&e.task_id)) {
                let what = if e.detail.is_empty() { e.kind.word().to_owned() } else { format!("{} {}", e.kind.word(), e.detail) };
                events.push((e.at_ms, short_id(&e.task_id), what));
            }
        }
        events.sort_by_key(|e| std::cmp::Reverse(e.0));
        let mut rows = v_flex().id("console-events").flex_1().min_h_0().overflow_y_scroll();
        for (i, (at, id, text)) in events.into_iter().take(150).enumerate() {
            rows = rows.child(
                h_flex()
                    .h(px(ROW))
                    .flex_shrink_0()
                    .items_center()
                    .when(i % 2 == 1, |d| d.bg(c(ZEBRA)))
                    .child(cell(clock(at), if now.saturating_sub(at) < 60_000 { WHITE } else { DIM }, Some(70.)))
                    .child(cell(id, WHITE, Some(58.)))
                    .child(cell(text, AMBER, None)),
            );
        }
        panel().size_full().child(bar("EVENTS".into(), String::new(), BAR_RED, WHITE)).child(rows).into_any_element()
    }

    pub(crate) fn console_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let typed = self.console_input.read(cx).value().trim().to_owned();
        if typed.is_empty() {
            return;
        }
        let said = self.console_do(&typed, window, cx);
        if said.0 {
            self.console_input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.console_said = Some(said);
        cx.notify();
    }

    fn console_find(&self, word: &str) -> Option<String> {
        let w = word.trim_start_matches('#').to_lowercase();
        if w.len() < 2 {
            return None;
        }
        let hits: Vec<&Task> = self.tasks.iter().filter(|t| t.id.to_lowercase().starts_with(&w)).collect();
        (hits.len() == 1).then(|| hits[0].id.clone())
    }

    fn console_target<'a>(&self, rest: &'a str) -> Option<(String, &'a str)> {
        let first = rest.split_whitespace().next().unwrap_or("");
        if let Some(id) = self.console_find(first) {
            return Some((id, rest[first.len()..].trim()));
        }
        self.console_selected().map(|t| (t.id.clone(), rest))
    }

    fn console_do(&mut self, typed: &str, window: &mut Window, cx: &mut Context<Self>) -> (bool, String) {
        let (verb, rest) = typed.split_once(char::is_whitespace).map_or((typed, ""), |(v, r)| (v, r.trim()));
        let verb = verb.to_lowercase();
        if let Ok(n) = verb.parse::<usize>()
            && rest.is_empty()
        {
            return match self.console_selected().map(|t| (t.id.clone(), t.options.len())) {
                Some((id, count)) if n >= 1 && n <= count => {
                    self.act_task(&id, TaskAct::Choose { index: n - 1 }, cx);
                    (true, format!("chose {n} for {}", short_id(&id)))
                }
                _ => (false, "no option with that number".into()),
            };
        }
        let act = |this: &mut Shell, act: TaskAct, word: &str, cx: &mut Context<Shell>| match this.console_target(rest) {
            Some((id, _)) => {
                this.act_task(&id, act, cx);
                (true, format!("{word} {}", short_id(&id)))
            }
            None => (false, "select a task or give its id".into()),
        };
        match verb.as_str() {
            "l" | "land" => act(self, TaskAct::Land, "landing", cx),
            "d" | "drop" => {
                let r = act(self, TaskAct::Drop, "dropped", cx);
                self.stage_pick = None;
                r
            }
            "s" | "start" | "ready" => act(self, TaskAct::Ready, "started", cx),
            "plan" => act(self, TaskAct::Plan, "planning", cx),
            "clear" if rest == "done" => {
                let ids: Vec<String> = self.tasks.iter().filter(|t| t.state == TaskState::Landed && !t.cleared).map(|t| t.id.clone()).collect();
                for id in &ids {
                    self.act_task(id, TaskAct::Clear, cx);
                }
                (true, format!("cleared {}", ids.len()))
            }
            "clear" => act(self, TaskAct::Clear, "cleared", cx),
            "choose" | "pick" => match self.console_target(rest) {
                Some((id, n)) => match n.trim().parse::<usize>() {
                    Ok(n) if n >= 1 => {
                        self.act_task(&id, TaskAct::Choose { index: n - 1 }, cx);
                        (true, format!("chose {n} for {}", short_id(&id)))
                    }
                    _ => (false, "choose [id] N".into()),
                },
                None => (false, "select a task or give its id".into()),
            },
            "t" | "tell" | "change" => match self.console_target(rest) {
                Some((id, text)) if !text.is_empty() => {
                    self.act_task(&id, TaskAct::Change { text: text.to_owned() }, cx);
                    (true, format!("told {}", short_id(&id)))
                }
                _ => (false, "t <what to change>".into()),
            },
            "o" | "open" => match self.console_target(rest).and_then(|(id, _)| self.tasks.iter().find(|t| t.id == id).and_then(|t| t.session)) {
                Some(s) => {
                    self.factory_view = false;
                    self.switch_open(s, false, cx);
                    (true, "opened its chat".into())
                }
                None => (false, "it has no chat".into()),
            },
            "new" | "do" if rest.is_empty() => {
                self.stage_pick = Some("new".into());
                (true, "write it, then START".into())
            }
            "new" | "do" | "add" => match self.board_repo() {
                Some(_) => {
                    self.task_title.update(cx, |s, cx| s.set_value(rest.to_owned(), window, cx));
                    self.add_task(verb != "add", window, cx);
                    (true, if verb == "add" { "saved as a draft".into() } else { "started".into() })
                }
                None => (false, "pick a project first: project <name>".into()),
            },
            "issues" => {
                self.stage_pick = Some("issues".into());
                self.ask_issues();
                (true, "asking github".into())
            }
            "solve" => match (self.board_repo(), rest.trim_start_matches('#').parse::<u64>()) {
                (Some(repo), Ok(n)) => {
                    self.cmd(Cmd::Ask { tag: "task", req: Request::TaskFromIssue { repo, issue: format!("#{n}"), ready: true } });
                    (true, format!("solving #{n}"))
                }
                _ => (false, "solve <issue number>".into()),
            },
            "rules" => {
                self.stage_pick = Some("rules".into());
                self.ask_rules();
                (true, "reading rules".into())
            }
            "teach" if !rest.is_empty() => {
                self.board_teach.update(cx, |s, cx| s.set_value(rest.to_owned(), window, cx));
                self.teach_board(window, cx);
                self.stage_pick = Some("rules".into());
                self.ask_rules();
                (true, "teaching it".into())
            }
            "help" | "?" => {
                self.stage_pick = Some("help".into());
                (true, String::new())
            }
            "pause" => {
                self.set_paused(true, window, cx);
                (true, "paused".into())
            }
            "resume" => {
                self.set_paused(false, window, cx);
                (true, "running".into())
            }
            "checkup" => match self.board_repo() {
                Some(repo) => {
                    self.checkup = Some((repo.clone(), None));
                    self.cmd(Cmd::Ask { tag: "checkup", req: Request::Checkup { repo } });
                    (true, "checkup started".into())
                }
                None => (false, "pick a project first: project <name>".into()),
            },
            "slack" => match self.board_repo() {
                Some(repo) => {
                    let channel = rest.split_whitespace().next().unwrap_or("").to_owned();
                    if channel.is_empty() {
                        return (false, "slack <channel> [since …] [note]".into());
                    }
                    let since = super::slack::since_in(rest, chrono::Local::now());
                    self.slack_run = Some(None);
                    self.cmd(Cmd::Ask { tag: "slack.read", req: Request::SlackRead { repo, channel, since, note: rest.to_owned(), ready: false } });
                    (true, "reading slack…".into())
                }
                None => (false, "pick a project first: project <name>".into()),
            },
            "project" => {
                if rest.is_empty() || rest == "all" {
                    self.board_project = None;
                    return (true, "all projects".into());
                }
                let want = rest.to_lowercase();
                match self.board_projects().into_iter().find(|p| super::projects::folder_name(p).to_lowercase() == want || p.to_lowercase().ends_with(&want)) {
                    Some(p) => {
                        self.board_project = Some(p.clone());
                        self.board_issues = None;
                        self.board_rules = None;
                        (true, super::projects::folder_name(&p))
                    }
                    None => (false, format!("no project {rest}: help lists them")),
                }
            }
            "host" => {
                let name = if rest.is_empty() || rest == "mac" { "mac".to_owned() } else { rest.to_owned() };
                match self.exec("host.use", &serde_json::json!({"host": name}), window, cx) {
                    Ok(_) => (true, format!("on {name}")),
                    Err(e) => (false, e),
                }
            }
            "mode" => {
                let (word, text) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(w, t)| (w, t.trim()));
                let body = match (word, MODES.iter().find(|(w, _)| *w == word)) {
                    ("rule", _) if !text.is_empty() => serde_json::json!({"autonomy": "custom", "bother": text}),
                    (_, Some((_, v))) => serde_json::json!({"autonomy": v}),
                    _ => return (false, "mode anything|critical|ui|never|rule <text>".into()),
                };
                match self.exec("daemon.set", &body, window, cx) {
                    Ok(_) => (true, format!("bother: {word}")),
                    Err(e) => (false, e),
                }
            }
            "dest" => {
                let (word, name) = rest.split_once(char::is_whitespace).map_or((rest, ""), |(w, n)| (w, n.trim()));
                match word {
                    "main" | "pr" | "keep" => {
                        self.board_land = Some(word.to_owned());
                        (true, format!("new tasks land: {word}"))
                    }
                    "branch" if !name.is_empty() => {
                        self.board_land = Some("branch".into());
                        self.board_branch.update(cx, |s, cx| s.set_value(name.to_owned(), window, cx));
                        (true, format!("new tasks land on {name}"))
                    }
                    _ => (false, "dest main|pr|keep|branch <name>".into()),
                }
            }
            "chats" => {
                let _ = self.exec("app.view", &serde_json::json!({"view": "chats"}), window, cx);
                (true, String::new())
            }
            "full" => {
                self.console_toggle_full(window, cx);
                (true, String::new())
            }
            _ => (false, "unknown: type help".into()),
        }
    }
}
