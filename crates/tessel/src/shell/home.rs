use super::*;
use gpui_kit::component::Icon;
use tessel_proto::proof::Verdict;
use tessel_proto::task::TaskState;

const TILE: f32 = 212.;
const GAP: f32 = 22.;
const ROUND: f32 = 46.;
const FACE: &str = theme::DEFAULT_FONT;

fn hex(v: u32) -> Hsla {
    rgb(v).into()
}

fn wash(from: u32, to: u32) -> Background {
    linear_gradient(160., linear_color_stop(hex(from), 0.0), linear_color_stop(hex(to), 1.0))
}

fn face(size: f32) -> Div {
    div().font_family(FACE).font_weight(FontWeight::BOLD).text_size(theme::z(size)).line_height(theme::z(size * 0.96))
}

fn words(size: f32, weight: FontWeight) -> Div {
    div().font_family(FACE).font_weight(weight).text_size(theme::z(size)).line_height(theme::z(size * 1.3))
}

fn tile(id: &str, wide: bool) -> Stateful<Div> {
    let w = if wide { TILE * 2. + GAP } else { TILE };
    v_flex()
        .id(SharedString::from(format!("home-{id}")))
        .relative()
        .flex_shrink_0()
        .w(theme::z(w))
        .h(theme::z(TILE))
        .p(theme::z(20.))
        .rounded(theme::z(ROUND))
        .overflow_hidden()
        .shadow_lg()
}

fn money(v: f64) -> String {
    if v > 0.0 && v < 0.01 { "<1¢".into() } else { format!("${v:.2}") }
}

fn ring(used: f32, track: Hsla, fill: Hsla) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let c = bounds.center();
            let r = f32::from(bounds.size.width.min(bounds.size.height)) / 2.0 - 7.0;
            let arc = |from: f32, len: f32, color: Hsla, window: &mut Window| {
                if len <= 0.0 {
                    return;
                }
                let mut b = PathBuilder::stroke(px(11.));
                let n = 64;
                for i in 0..=n {
                    let a = (from + len * i as f32 / n as f32 - 0.25) * std::f32::consts::TAU;
                    let p = point(c.x + px(r * a.cos()), c.y + px(r * a.sin()));
                    if i == 0 { b.move_to(p) } else { b.line_to(p) }
                }
                if let Ok(path) = b.build() {
                    window.paint_path(path, color);
                }
            };
            arc(0.0, 1.0, track, window);
            arc(0.0, used.clamp(0.0, 1.0), fill, window);
        },
    )
    .size_full()
}

impl Shell {
    pub(crate) fn home_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let white = hex(0xffffff);
        let now = super::now_ms();

        let busy: Vec<&tessel_proto::SessionSummary> = self.model.sessions.iter().filter(|s| s.run_state.is_busy()).collect();
        let working = tile("working", false)
            .bg(wash(0xff9f43, 0xff4f7b))
            .text_color(white)
            .child(words(15., FontWeight::SEMIBOLD).opacity(0.9).child("Working"))
            .child(face(112.).mt(theme::z(4.)).child(busy.len().to_string()))
            .child(div().flex_1())
            .child(words(13., FontWeight::MEDIUM).opacity(0.9).truncate().child(match busy.first() {
                Some(s) => s.activity.clone().map(|a| super::sentence(&a)).unwrap_or_else(|| s.goal.clone()),
                None => "All quiet".to_owned(),
            }));

        let review: Vec<&tessel_proto::task::Task> = self.tasks.iter().filter(|t| t.state == TaskState::Review).collect();
        let asking = self.model.sessions.iter().filter(|s| s.attention.is_some()).count();
        let waiting_n = review.len().max(asking);
        let mut waiting = tile("waiting", false)
            .bg(wash(0xffffff, 0xf2f2f7))
            .text_color(hex(0x1c1c1e))
            .child(words(15., FontWeight::SEMIBOLD).text_color(hex(0xff3b30)).child("Waiting on you"))
            .child(face(96.).mt(theme::z(2.)).child(waiting_n.to_string()))
            .child(div().flex_1());
        let mut names: Vec<String> = review.iter().map(|t| t.title.clone()).collect();
        for s in self.model.sessions.iter().filter(|s| s.attention.is_some()) {
            if !names.contains(&s.goal) {
                names.push(s.goal.clone());
            }
        }
        names.truncate(2);
        for n in names {
            waiting = waiting.child(
                h_flex()
                    .gap(theme::z(7.))
                    .items_center()
                    .child(div().size(theme::z(7.)).flex_shrink_0().rounded_full().bg(hex(0xff3b30)))
                    .child(words(13., FontWeight::MEDIUM).min_w_0().truncate().text_color(hex(0x3a3a3c)).child(n)),
            );
        }

        let days = super::views::spend_by_day(&self.metrics.as_ref().map(|m| m.spend.clone()).unwrap_or_default(), now, 4);
        let today = days.last().map_or(0.0, |d| d.1);
        let mut row = h_flex().w_full().justify_between();
        for (label, v) in &days {
            row = row.child(
                v_flex()
                    .items_center()
                    .gap(theme::z(2.))
                    .child(words(13., FontWeight::SEMIBOLD).child(money(*v)))
                    .child(words(11., FontWeight::MEDIUM).opacity(0.8).child(label.clone())),
            );
        }
        let spend = tile("spend", false)
            .bg(wash(0x4a9df8, 0x9ccbf7))
            .text_color(white)
            .child(words(15., FontWeight::SEMIBOLD).opacity(0.9).child("Today, at API prices"))
            .child(face(if today >= 100.0 { 40. } else if today >= 10.0 { 46. } else { 54. }).mt(theme::z(8.)).whitespace_nowrap().child(money(today)))
            .child(div().flex_1())
            .child(row);

        let m = self.metrics.clone().unwrap_or_default();
        let landed = tile("landed", false)
            .bg(wash(0x2f9e5d, 0x1b6e3e))
            .text_color(white)
            .child(words(15., FontWeight::SEMIBOLD).opacity(0.9).child("Landed"))
            .child(face(112.).mt(theme::z(4.)).child(m.landed.to_string()))
            .child(div().flex_1())
            .child(words(13., FontWeight::MEDIUM).opacity(0.9).child(if m.landed > 0 { format!("{} each", money(m.cost_per_landed)) } else { "Nothing yet".to_owned() }));

        let window = self.usage.iter().filter(|w| w.resets_at.is_none_or(|r| r * 1000 > now as i64)).max_by(|a, b| a.used.total_cmp(&b.used));
        let used = window.map_or(0.0, |w| w.used) as f32;
        let resets = window.and_then(|w| w.resets_at).map(|r| {
            let mins = ((r * 1000 - now as i64).max(0) / 60_000) as u64;
            if mins >= 60 { format!("Resets in {}h", mins / 60) } else { format!("Resets in {mins}m") }
        });
        let tone = if used >= 0.9 { hex(0xff453a) } else if used >= 0.7 { hex(0xff9f0a) } else { hex(0x30d158) };
        let plan = tile("plan", false)
            .bg(wash(0x2c2c2e, 0x1c1c1e))
            .text_color(white)
            .items_center()
            .child(
                div()
                    .relative()
                    .size(theme::z(132.))
                    .child(ring(used, white.opacity(0.14), tone))
                    .child(v_flex().absolute().inset_0().items_center().justify_center().child(face(38.).child(format!("{:.0}%", used * 100.))).child(words(11., FontWeight::SEMIBOLD).opacity(0.7).child("Claude"))),
            )
            .child(div().flex_1())
            .child(words(13., FontWeight::MEDIUM).opacity(0.85).child(resets.unwrap_or_else(|| "Plan usage".into())));

        let shot = self
            .model
            .sessions
            .iter()
            .filter_map(|s| s.proof.as_ref().filter(|pf| pf.verdict.done()).and_then(|pf| pf.shots.first().map(|path| (s, pf, path.clone()))))
            .max_by_key(|(_, pf, _)| pf.finished_ms.unwrap_or(pf.started_ms));
        let photo = shot.map(|(s, pf, path)| {
            let picture: Option<ImageSource> = if std::path::Path::new(&path).is_file() {
                Some(std::path::PathBuf::from(&path).into())
            } else {
                self.shots.get(&path).cloned().flatten().map(ImageSource::from)
            };
            let word = match pf.verdict {
                Verdict::Proven => "Proven",
                Verdict::Concerns => "Proven, with concerns",
                Verdict::Failed => "Failed",
                _ => "Checked",
            };
            let session = s.id;
            tile("photo", true)
                .p_0()
                .bg(hex(0x1c1c1e))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.switch_open(session, false, cx);
                    cx.notify();
                }))
                .when_some(picture, |d, src| d.child(img(src).absolute().inset_0().size_full().rounded(theme::z(ROUND)).object_fit(ObjectFit::Cover)))
                .child(div().absolute().inset_0().rounded(theme::z(ROUND)).bg(linear_gradient(180., linear_color_stop(hex(0x000000).opacity(0.0), 0.45), linear_color_stop(hex(0x000000).opacity(0.62), 1.0))))
                .child(
                    v_flex()
                        .absolute()
                        .left(theme::z(22.))
                        .right(theme::z(22.))
                        .bottom(theme::z(20.))
                        .text_color(white)
                        .child(face(24.).truncate().child(s.goal.clone()))
                        .child(words(13., FontWeight::MEDIUM).opacity(0.85).child(format!("{word} · {} files · {}", pf.files, money(pf.cost_usd)))),
                )
        });

        let shortcut = |id: &'static str, icon: &'static str, label: &'static str| {
            v_flex()
                .id(id)
                .test_support()
                .flex_1()
                .h_full()
                .p(theme::z(12.))
                .justify_between()
                .rounded(theme::z(22.))
                .bg(white.opacity(0.2))
                .cursor_pointer()
                .hover(move |d| d.bg(white.opacity(0.3)))
                .child(h_flex().size(theme::z(30.)).justify_center().rounded_full().bg(white.opacity(0.9)).text_color(hex(0x5e4ae3)).child(Icon::empty().path(format!("icons/{icon}.svg")).size(theme::z(16.))))
                .child(words(13., FontWeight::SEMIBOLD).child(label))
        };
        let open_board = cx.listener(|this, _, window, cx| {
            let _ = this.exec("app.factory", &serde_json::json!({"on": true}), window, cx);
            let _ = this.exec("pane.show", &serde_json::json!({"module": "board"}), window, cx);
        });
        let new_chat = cx.listener(|this, _, _, cx| {
            this.show_new_session(cx);
            cx.notify();
        });
        let prove = cx.listener(|this, _, _, cx| {
            if let Some(s) = this.model.open_summary().map(|s| s.id) {
                this.prove(s, false, cx);
            }
        });
        let paused = self.factory_paused();
        let pause = cx.listener(move |this, _, window, cx| this.set_paused(!paused, window, cx));
        let controls = tile("controls", false)
            .p(theme::z(12.))
            .gap(theme::z(10.))
            .bg(wash(0x7b68ff, 0x5e4ae3))
            .text_color(white)
            .child(h_flex().flex_1().gap(theme::z(10.)).child(shortcut("home-new", "square-pen", "New Chat").on_click(new_chat)).child(shortcut("home-board", "square-kanban", "Board").on_click(open_board)))
            .child(h_flex().flex_1().gap(theme::z(10.)).child(shortcut("home-prove", "shield-check", "Prove").on_click(prove)).child(shortcut("home-pause", if paused { "play" } else { "pause" }, if paused { "Resume" } else { "Pause" }).on_click(pause)));

        let date = {
            use chrono::TimeZone as _;
            chrono::Local.timestamp_millis_opt(now as i64).single().map(|t| t.format("%A, %B %-d").to_string()).unwrap_or_default()
        };
        let grid = h_flex()
            .w_full()
            .max_w(theme::z(TILE * 4. + GAP * 3.))
            .flex_wrap()
            .justify_center()
            .gap(theme::z(GAP))
            .child(working)
            .child(waiting)
            .children(photo)
            .child(spend)
            .child(landed)
            .child(plan)
            .child(controls);
        v_flex()
            .id("home")
            .size_full()
            .overflow_y_scroll()
            .items_center()
            .px(theme::z(32.))
            .pt(theme::z(40.))
            .pb(theme::z(56.))
            .gap(theme::z(28.))
            .child(
                v_flex()
                    .w_full()
                    .max_w(theme::z(TILE * 4. + GAP * 3.))
                    .child(words(15., FontWeight::SEMIBOLD).text_color(p.accent).child(date.to_uppercase()))
                    .child(face(40.).text_color(p.label).child("Home")),
            )
            .child(grid)
            .into_any_element()
    }
}
