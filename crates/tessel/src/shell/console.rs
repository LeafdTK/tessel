use super::*;
use tessel_proto::task::{Task, TaskAct, TaskState};
use tessel_proto::Request;

const BG: u32 = 0x000000;
const PANEL: u32 = 0x000000;
const RULE: u32 = 0x262626;
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
    v_flex().min_w_0().min_h_0().overflow_hidden().bg(c(PANEL))
}

fn cell(text: impl Into<SharedString>, color: u32, w: Option<f32>) -> Div {
    let d = div().px(px(6.)).truncate().text_color(c(color)).child(text.into());
    match w {
        Some(w) => d.w(px(w)).flex_shrink_0(),
        None => d.flex_1().min_w_0(),
    }
}

impl Shell {
    fn console_tasks(&self) -> Vec<&Task> {
        let rank = |s: TaskState| match s {
            TaskState::Review => 0,
            TaskState::Running | TaskState::Planned => 1,
            TaskState::Proving | TaskState::Landing | TaskState::Checking => 2,
            TaskState::Ready => 3,
            TaskState::Landed => 4,
            TaskState::Draft => 5,
            TaskState::Dropped => 6,
            _ => 7,
        };
        let mut v: Vec<&Task> = self.tasks.iter().filter(|t| !t.cleared && self.board_project.as_ref().is_none_or(|r| &t.repo == r)).filter(|t| t.state != TaskState::Dropped || super::now_ms().saturating_sub(t.updated_ms) < 86_400_000).collect();
        v.sort_by_key(|t| (rank(t.state), std::cmp::Reverse(t.updated_ms)));
        v
    }

    pub(crate) fn console(&self, cx: &mut Context<Self>) -> AnyElement {
        let now = super::now_ms();
        let tasks = self.console_tasks();
        let live: Vec<&Task> = tasks.iter().copied().filter(|t| matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking) && t.session.is_some()).take(3).collect();
        let needs: Vec<&Task> = tasks.iter().copied().filter(|t| t.state == TaskState::Review).collect();
        let mut top = h_flex().h(px(262.)).flex_shrink_0().gap(px(2.)).child(div().flex_1().min_w_0().h_full().child(self.console_board(&tasks, now, cx))).child(div().w(px(330.)).flex_shrink_0().h_full().child(self.console_gauges(now)));
        top = top.min_w_0();
        let mut lower_left = v_flex().flex_1().min_w_0().min_h_0().gap(px(2.));
        if !live.is_empty() {
            let mut feeds = h_flex().flex_1().min_h_0().gap(px(2.));
            for t in &live {
                feeds = feeds.child(self.console_feed(t, now, cx));
            }
            lower_left = lower_left.child(feeds);
        }
        if !needs.is_empty() {
            lower_left = lower_left.child(self.console_needs(&needs, cx));
        }
        if live.is_empty() && needs.is_empty() {
            lower_left = lower_left.child(panel().flex_1().child(bar("LIVE".into(), String::new(), BAR_BLUE, WHITE)).child(div().p(px(8.)).text_color(c(GREY)).child("Nothing running.")));
        }
        v_flex()
            .id("console")
            .test_support()
            .size_full()
            .min_w_0()
            .bg(c(BG))
            .p(px(2.))
            .gap(px(2.))
            .font_family(theme::mono())
            .text_size(px(TEXT))
            .line_height(px(ROW - 3.))
            .text_color(c(AMBER))
            .child(self.console_command(cx))
            .child(top)
            .child(h_flex().flex_1().min_h_0().gap(px(2.)).child(lower_left).child(div().w(px(330.)).flex_shrink_0().h_full().child(self.console_events(now))))
            .into_any_element()
    }

    fn console_command(&self, cx: &mut Context<Self>) -> AnyElement {
        let paused = self.factory_paused();
        let host = self.hosts.list.current().map_or_else(|| "this mac".to_owned(), |h| h.name.clone());
        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .px(px(6.))
            .gap(px(10.))
            .items_center()
            .bg(c(0x1a1a1a))
            .child(div().text_color(c(YELLOW)).font_weight(FontWeight::BOLD).child("TESSEL"))
            .child(div().text_color(c(GREY)).child(host))
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
            .children(self.console_said.clone().map(|(ok, said)| div().max_w(px(420.)).truncate().text_color(c(if ok { GREEN } else { RED })).child(said)))
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
            .into_any_element()
    }

    fn console_board(&self, tasks: &[&Task], now: u64, cx: &mut Context<Self>) -> AnyElement {
        let count = |f: &dyn Fn(&Task) -> bool| tasks.iter().filter(|t| f(t)).count();
        let summary = [
            (count(&|t| t.state == TaskState::Review), "NEED"),
            (count(&|t| matches!(t.state, TaskState::Running | TaskState::Planned)), "WORK"),
            (count(&|t| matches!(t.state, TaskState::Proving | TaskState::Landing | TaskState::Checking)), "CHECK"),
            (count(&|t| t.state == TaskState::Ready), "NEXT"),
        ]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, w)| format!("{n} {w}"))
        .collect::<Vec<_>>()
        .join("  ");
        let project = self.board_project.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "all".into());
        let head = h_flex()
            .h(px(ROW))
            .flex_shrink_0()
            .text_color(c(GREY))
            .border_b_1()
            .border_color(c(RULE))
            .child(cell("ID", GREY, Some(64.)))
            .child(cell("TASK", GREY, None))
            .child(cell("ST", GREY, Some(64.)))
            .child(cell("AGE", GREY, Some(44.)))
            .child(cell("NOW", GREY, None))
            .child(cell("COST", GREY, Some(62.)))
            .child(cell("DIFF", GREY, Some(100.)))
            .child(cell("LINK", GREY, Some(96.)));
        let mut rows = v_flex().id("console-board-rows").flex_1().min_h_0().overflow_y_scroll();
        for (i, t) in tasks.iter().enumerate() {
            let (word, tone) = state_word(t);
            let now_text = match t.state {
                TaskState::Review => t.ask.clone().unwrap_or_else(|| self.outcome(t)),
                TaskState::Landed | TaskState::Dropped => self.outcome(t),
                TaskState::Ready if !t.deps.is_empty() => format!("after {}", t.deps.iter().map(|d| short_id(d)).collect::<Vec<_>>().join(", ")),
                _ => self.stage_doing(t),
            };
            let cost = self.task_cost(t);
            let (added, removed) = t.changes.iter().fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
            let link: Option<(String, String)> = match (&t.pr, &t.link, &t.landed) {
                (Some(pr), _, _) => Some((pr.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "PR".to_owned(), |n| format!("PR #{n}")), pr.clone())),
                (None, Some(url), Some(l)) => Some((l.chars().take(7).collect(), url.clone())),
                _ => None,
            };
            let id = t.id.clone();
            rows = rows.child(
                h_flex()
                    .id(SharedString::from(format!("console-row-{}", t.id)))
                    .h(px(ROW))
                    .flex_shrink_0()
                    .items_center()
                    .when(i % 2 == 1, |d| d.bg(c(0x0b0b0b)))
                    .hover(|d| d.bg(c(0x1a1a1a)))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stage_pick = Some(id.clone());
                        cx.notify();
                    }))
                    .child(cell(short_id(&t.id), WHITE, Some(64.)))
                    .child(cell(t.title.clone(), WHITE, None))
                    .child(cell(word, tone, Some(64.)))
                    .child(cell(short_age(t.created_ms, now), AMBER, Some(44.)))
                    .child(cell(now_text, GREY, None))
                    .child(cell(if cost >= 0.005 { format!("${cost:.2}") } else { "—".into() }, AMBER, Some(62.)))
                    .child(
                        h_flex()
                            .w(px(100.))
                            .flex_shrink_0()
                            .px(px(6.))
                            .gap(px(6.))
                            .when(added + removed > 0, |d| d.child(div().text_color(c(GREEN)).child(format!("+{added}"))).child(div().text_color(c(RED)).child(format!("−{removed}"))))
                            .when(added + removed == 0, |d| d.child(div().text_color(c(DIM)).child("—"))),
                    )
                    .child(match link {
                        Some((label, url)) => div()
                            .id(SharedString::from(format!("console-link-{}", t.id)))
                            .w(px(96.))
                            .flex_shrink_0()
                            .px(px(6.))
                            .truncate()
                            .text_color(c(CYAN))
                            .cursor_pointer()
                            .hover(|d| d.underline())
                            .on_click(move |_, _, cx| {
                                cx.stop_propagation();
                                cx.open_url(&url);
                            })
                            .child(label)
                            .into_any_element(),
                        None => cell("", DIM, Some(96.)).into_any_element(),
                    }),
            );
        }
        if tasks.is_empty() {
            rows = rows.child(div().p(px(8.)).text_color(c(GREY)).child("No tasks. Type one above, like: new fix the login button"));
        }
        panel().size_full().child(bar(format!("BOARD  {project}"), summary, BAR_RED, WHITE)).child(head).child(rows).into_any_element()
    }

    fn console_gauges(&self, now: u64) -> AnyElement {
        let day = 86_400_000u64;
        let today_start = {
            use chrono::TimeZone as _;
            let n = chrono::Local::now();
            n.date_naive().and_hms_opt(0, 0, 0).and_then(|d| chrono::Local.from_local_datetime(&d).single()).map_or(now.saturating_sub(day), |d| d.timestamp_millis() as u64)
        };
        let mine = |t: &&Task| self.board_project.as_ref().is_none_or(|r| &t.repo == r);
        let landed_today: Vec<&Task> = self.tasks.iter().filter(mine).filter(|t| t.state == TaskState::Landed && t.updated_ms >= today_start).collect();
        let per = if landed_today.is_empty() { None } else { Some(landed_today.iter().map(|t| self.task_cost(t)).sum::<f64>() / landed_today.len() as f64) };
        let recent: Vec<&Task> = self.tasks.iter().filter(mine).filter(|t| now.saturating_sub(t.updated_ms) < day && matches!(t.state, TaskState::Landed | TaskState::Dropped)).collect();
        let success = (!recent.is_empty()).then(|| recent.iter().filter(|t| t.state == TaskState::Landed).count() * 100 / recent.len());
        let limit = |name: &str| self.usage.iter().find(|w| w.name == name).map(|w| if w.resets_at.is_some_and(|r| (r as u64) * 1000 <= now) { 0 } else { (w.used * 100.0).round() as u32 });
        let mut hours = [0u32; 12];
        for t in self.tasks.iter().filter(mine).filter(|t| t.state == TaskState::Landed) {
            let ago = now.saturating_sub(t.updated_ms) / 3_600_000;
            if (ago as usize) < hours.len() {
                hours[hours.len() - 1 - ago as usize] += 1;
            }
        }
        let peak = hours.iter().copied().max().unwrap_or(0).max(1) as f32;
        let tile = |label: &str, value: String, tone: u32, note: String| {
            v_flex()
                .flex_1()
                .min_w_0()
                .p(px(6.))
                .gap(px(2.))
                .bg(c(0x0b0b0b))
                .border_1()
                .border_color(c(RULE))
                .child(div().text_color(c(GREY)).child(label.to_owned()))
                .child(div().text_size(px(26.)).line_height(px(30.)).text_color(c(tone)).child(value))
                .child(div().text_color(c(DIM)).truncate().child(note))
        };
        let pct = |v: Option<u32>| v.map_or("—".to_owned(), |v| format!("{v}%"));
        let tone = |v: Option<u32>| match v {
            Some(v) if v >= 90 => RED,
            Some(v) if v >= 70 => YELLOW,
            _ => WHITE,
        };
        let resets = |name: &str| self.usage.iter().find(|w| w.name == name).and_then(|w| w.resets_at).filter(|r| (*r as u64) * 1000 > now).map(|r| format!("resets {}", super::composer::resets(r))).unwrap_or_default();
        let (five, week) = (limit("five_hour"), limit("seven_day"));
        let mut spark = h_flex().h(px(30.)).items_end().gap(px(2.));
        for n in hours {
            spark = spark.child(div().flex_1().h(px(2. + 28. * n as f32 / peak)).bg(c(if n > 0 { GREEN } else { 0x1f1f1f })));
        }
        panel()
            .size_full()
            .child(bar("TODAY".into(), String::new(), BAR_ORANGE, 0x000000))
            .child(
                v_flex()
                    .flex_1()
                    .p(px(2.))
                    .gap(px(2.))
                    .child(h_flex().gap(px(2.)).child(tile("LANDED", landed_today.len().to_string(), WHITE, String::new())).child(tile("COST/LANDED", per.map_or("—".into(), |v| format!("${v:.2}")), WHITE, String::new())))
                    .child(h_flex().gap(px(2.)).child(tile("5H LIMIT", pct(five), tone(five), resets("five_hour"))).child(tile("WEEKLY", pct(week), tone(week), resets("seven_day"))))
                    .child(h_flex().gap(px(2.)).child(tile("SUCCESS 24H", success.map_or("—".into(), |s| format!("{s}%")), WHITE, String::new())).child(v_flex().flex_1().min_w_0().p(px(6.)).gap(px(4.)).bg(c(0x0b0b0b)).border_1().border_color(c(RULE)).child(div().text_color(c(GREY)).child("LANDED/HR 12H")).child(spark))),
            )
            .into_any_element()
    }

    fn console_feed(&self, t: &Task, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let (word, tone) = state_word(t);
        let steps = t.session.map(|s| self.steps(s, 24)).unwrap_or_default();
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
            .child(div().h(px(1.)).bg(c(tone)))
            .child(col)
            .into_any_element()
    }

    fn console_needs(&self, needs: &[&Task], cx: &mut Context<Self>) -> AnyElement {
        let mut col = v_flex().p(px(6.)).gap(px(6.));
        for t in needs.iter().take(3) {
            let id = t.id.clone();
            let mut actions = h_flex().gap(px(14.));
            if t.options.is_empty() {
                if !t.changes.is_empty() || !t.shots.is_empty() {
                    let i = id.clone();
                    actions = actions.child(div().id(SharedString::from(format!("console-land-{id}"))).text_color(c(GREEN)).cursor_pointer().hover(|d| d.underline()).on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))).child("[L] LAND <GO>"));
                }
            } else {
                for (n, o) in t.options.iter().enumerate() {
                    let i = id.clone();
                    actions = actions.child(div().id(SharedString::from(format!("console-choose-{id}-{n}"))).text_color(c(if n == 0 { GREEN } else { CYAN })).cursor_pointer().hover(|d| d.underline()).on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Choose { index: n }, cx))).child(format!("[{}] {} <GO>", n + 1, o.label)));
                }
            }
            let i = id.clone();
            actions = actions.child(div().id(SharedString::from(format!("console-open-{id}"))).text_color(c(GREY)).cursor_pointer().hover(|d| d.underline()).on_click(cx.listener(move |this, _, _, cx| {
                this.stage_pick = Some(i.clone());
                cx.notify();
            })).child("[O] OPEN <GO>"));
            col = col.child(
                v_flex()
                    .child(h_flex().gap(px(8.)).child(div().text_color(c(WHITE)).child(short_id(&t.id))).child(div().flex_1().min_w_0().truncate().text_color(c(YELLOW)).child(t.title.clone())))
                    .child(div().text_color(c(WHITE)).child(t.ask.clone().unwrap_or_else(|| self.outcome(t))))
                    .child(actions),
            );
        }
        panel().flex_shrink_0().max_h(px(200.)).child(bar("NEEDS YOU".into(), needs.len().to_string(), BAR_ORANGE, 0x000000)).child(col).into_any_element()
    }

    fn console_events(&self, now: u64) -> AnyElement {
        let mut events: Vec<(u64, String, String)> = Vec::new();
        for t in self.tasks.iter().filter(|t| self.board_project.as_ref().is_none_or(|r| &t.repo == r)) {
            for n in &t.notes {
                events.push((n.at_ms, short_id(&t.id), super::sentence(&n.text)));
            }
        }
        events.sort_by_key(|e| std::cmp::Reverse(e.0));
        let mut rows = v_flex().id("console-events").flex_1().min_h_0().overflow_y_scroll();
        for (i, (at, id, text)) in events.into_iter().take(120).enumerate() {
            rows = rows.child(
                h_flex()
                    .h(px(ROW))
                    .flex_shrink_0()
                    .items_center()
                    .when(i % 2 == 1, |d| d.bg(c(0x0b0b0b)))
                    .child(cell(clock(at), if now.saturating_sub(at) < 60_000 { WHITE } else { DIM }, Some(70.)))
                    .child(cell(id, WHITE, Some(58.)))
                    .child(cell(text, AMBER, None)),
            );
        }
        panel().size_full().child(bar("EVENTS".into(), String::new(), BAR_RED, WHITE)).child(rows).into_any_element()
    }

    pub(crate) fn console_run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.console_input.read(cx).value().trim().to_owned();
        if line.is_empty() {
            return;
        }
        let said = self.console_do(&line, window, cx);
        if said.0 {
            self.console_input.update(cx, |s, cx| s.set_value("", window, cx));
        }
        self.console_said = Some(said);
        cx.notify();
    }

    fn console_find(&self, word: &str) -> Option<String> {
        let w = word.trim_start_matches('#').to_lowercase();
        let hits: Vec<&Task> = self.tasks.iter().filter(|t| t.id.to_lowercase().starts_with(&w)).collect();
        (hits.len() == 1).then(|| hits[0].id.clone())
    }

    fn console_do(&mut self, line: &str, window: &mut Window, cx: &mut Context<Self>) -> (bool, String) {
        let (verb, rest) = line.split_once(char::is_whitespace).map_or((line, ""), |(v, r)| (v, r.trim()));
        let act = |this: &mut Shell, rest: &str, act: TaskAct, word: &str, cx: &mut Context<Shell>| match this.console_find(rest) {
            Some(id) => {
                this.act_task(&id, act, cx);
                (true, format!("{word} {}", short_id(&id)))
            }
            None => (false, format!("no single task starts with {rest}")),
        };
        match verb.to_lowercase().as_str() {
            "drop" => act(self, rest, TaskAct::Drop, "dropped", cx),
            "start" | "ready" => act(self, rest, TaskAct::Ready, "started", cx),
            "land" => act(self, rest, TaskAct::Land, "landing", cx),
            "plan" => act(self, rest, TaskAct::Plan, "planning", cx),
            "clear" => act(self, rest, TaskAct::Clear, "cleared", cx),
            "choose" | "pick" => {
                let mut parts = rest.split_whitespace();
                match (parts.next().and_then(|w| self.console_find(w)), parts.next().and_then(|n| n.parse::<usize>().ok())) {
                    (Some(id), Some(n)) if n >= 1 => {
                        self.act_task(&id, TaskAct::Choose { index: n - 1 }, cx);
                        (true, format!("chose {n} for {}", short_id(&id)))
                    }
                    _ => (false, "choose ID N".into()),
                }
            }
            "change" | "tell" => match rest.split_once(char::is_whitespace) {
                Some((w, text)) => match self.console_find(w) {
                    Some(id) => {
                        self.act_task(&id, TaskAct::Change { text: text.trim().to_owned() }, cx);
                        (true, format!("told {}", short_id(&id)))
                    }
                    None => (false, format!("no single task starts with {w}")),
                },
                None => (false, "tell ID what to change".into()),
            },
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
                None => (false, "pick a project first".into()),
            },
            "slack" => match self.board_repo() {
                Some(repo) => {
                    let channel = rest.split_whitespace().next().unwrap_or("").to_owned();
                    if channel.is_empty() {
                        return (false, "slack CHANNEL [since …] [note]".into());
                    }
                    let since = super::slack::since_in(rest, chrono::Local::now());
                    self.slack_run = Some(None);
                    self.cmd(Cmd::Ask { tag: "slack.read", req: Request::SlackRead { repo, channel, since, note: rest.to_owned(), ready: false } });
                    (true, "reading slack…".into())
                }
                None => (false, "pick a project first".into()),
            },
            "new" | "add" | "do" => match self.board_repo() {
                Some(repo) if !rest.is_empty() => {
                    self.cmd(Cmd::Ask { tag: "task", req: Request::TaskCreate { repo, title: rest.to_owned(), spec: String::new(), deps: Vec::new(), parent: None, ready: verb != "add", budget_usd: None, plan: false, from: None, attachments: Vec::new(), land: None } });
                    (true, "filed".into())
                }
                Some(_) => (false, "new WHAT TO DO".into()),
                None => (false, "pick a project first".into()),
            },
            "open" => match self.console_find(rest) {
                Some(id) => {
                    self.stage_pick = Some(id.clone());
                    (true, format!("opened {}", short_id(&id)))
                }
                None => (false, format!("no single task starts with {rest}")),
            },
            _ => (false, "commands: new, add, start, drop, land, choose, tell, open, clear, slack, checkup, pause, resume".into()),
        }
    }
}
