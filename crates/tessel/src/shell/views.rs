use super::*;
use gpui_kit::component::Icon;
use serde_json::{Value, json};
use tessel_proto::task::{Task, TaskState};
use tessel_proto::{Request, Response};

const MAKE: &str = "Make me a tessel view: a screen of live numbers, charts and tables for what I describe below. Write it as a JSON file in ~/.tessel/views/ (name the file for what it shows), bind its elements to sources so it stays current by itself, and keep it to one screen. Then tell me in a line what it shows.";

pub(crate) fn factory_view() -> Value {
    let metric = |label: &str, bind: &str| json!({"k": "metric", "p": {"label": label, "bind": bind}});
    json!({"k": "column", "p": {"gap": 14}, "c": [
        {"k": "row", "p": {"gap": 24}, "c": [
            metric("Landed", "factory.landed"),
            metric("Waiting on you", "factory.review"),
            metric("Working", "factory.working"),
            metric("Per landed task", "factory.cost_per_landed"),
            metric("Wasted", "factory.waste"),
            metric("Reverted later", "factory.escaped")
        ]},
        {"k": "card", "p": {"title": "Spent each day"}, "c": [{"k": "bars", "p": {"bind": "cost.daily", "unit": "$"}}]},
        {"k": "card", "p": {"title": "Waiting on you"}, "c": [{"k": "table", "p": {"bind": "tasks.review"}}]}
    ]})
}

fn state_word(s: TaskState) -> String {
    serde_json::to_value(s).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

fn day_of(ms: u64) -> (i64, String) {
    let days = (ms / 86_400_000) as i64;
    let names = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"];
    (days, names[days.rem_euclid(7) as usize].to_owned())
}

impl Shell {
    pub(crate) fn views_answer(&mut self, result: Result<Response, String>) {
        if let Ok(Response::Views { views }) = result {
            self.saved_views = views;
        }
    }

    pub(crate) fn metrics_answer(&mut self, result: Result<Response, String>) {
        if let Ok(Response::Metrics { metrics }) = result {
            self.metrics = Some(metrics);
        }
    }

    pub(crate) fn refresh_views(&mut self) {
        self.cmd(Cmd::Ask { tag: "views", req: Request::Views });
        self.cmd(Cmd::Ask { tag: "metrics", req: Request::Metrics });
    }

    fn task_cost_of(&self, t: &Task) -> f64 {
        t.proof_cost_usd + t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).map_or(0.0, |s| s.cost_usd)
    }

    pub(crate) fn bound_props(&self, kind: &str, p: Value) -> Value {
        let Some(bind) = p.get("bind").and_then(Value::as_str).map(str::to_owned) else { return p };
        let mut out = p.clone();
        let mut put = |k: &str, v: Value| {
            if let Some(o) = out.as_object_mut() {
                o.insert(k.to_owned(), v);
            }
        };
        let money = |v: f64| format!("${v:.2}");
        match bind.as_str() {
            b if b == "tasks" || b.starts_with("tasks.") => {
                let want = b.strip_prefix("tasks.");
                let mut tasks: Vec<&Task> = self.tasks.iter().filter(|t| t.state != TaskState::Dropped && want.is_none_or(|w| state_word(t.state) == w || (w == "running" && t.state.active()))).collect();
                tasks.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
                match kind {
                    "metric" => put("value", json!(tasks.len())),
                    "list" => put("items", json!(tasks.iter().map(|t| json!({"title": t.title, "note": state_word(t.state)})).collect::<Vec<_>>())),
                    _ => {
                        put("columns", json!(["Task", "State", "Cost"]));
                        put("rows", json!(tasks.iter().map(|t| json!([t.title, state_word(t.state), money(self.task_cost_of(t))])).collect::<Vec<_>>()));
                    }
                }
            }
            "chats" => {
                let mut chats: Vec<&tessel_proto::SessionSummary> = self.model.sessions.iter().collect();
                chats.sort_by_key(|s| (!s.run_state.is_busy(), std::cmp::Reverse(s.updated_ms)));
                match kind {
                    "metric" => put("value", json!(chats.iter().filter(|s| s.run_state.is_busy()).count())),
                    "list" => put("items", json!(chats.iter().take(20).map(|s| json!({"title": s.goal, "note": format!("{:?}", s.run_state).to_lowercase()})).collect::<Vec<_>>())),
                    _ => {
                        put("columns", json!(["Chat", "State", "Cost"]));
                        put("rows", json!(chats.iter().take(40).map(|s| json!([s.goal, format!("{:?}", s.run_state).to_lowercase(), money(s.cost_usd)])).collect::<Vec<_>>()));
                    }
                }
            }
            "proofs" => {
                let proved: Vec<(&tessel_proto::SessionSummary, &tessel_proto::proof::Proof)> = self.model.sessions.iter().filter_map(|s| s.proof.as_ref().map(|p| (s, p))).collect();
                match kind {
                    "metric" => put("value", json!(format!("{}/{}", proved.iter().filter(|(_, p)| p.verdict == tessel_proto::proof::Verdict::Proven).count(), proved.len()))),
                    _ => {
                        put("columns", json!(["Chat", "Proof", "Cost"]));
                        put("rows", json!(proved.iter().map(|(s, p)| json!([s.goal, super::proof::verdict_word(p), money(p.cost_usd)])).collect::<Vec<_>>()));
                    }
                }
            }
            "cost.daily" => {
                let today = day_of(super::now_ms()).0;
                let mut by_day: Vec<(String, f64)> = (0..7).rev().map(|back| (day_of(((today - back) as u64) * 86_400_000).1, 0.0)).collect();
                for s in &self.model.sessions {
                    let (d, _) = day_of(s.updated_ms.max(s.created_ms));
                    let back = today - d;
                    if (0..7).contains(&back) {
                        by_day[(6 - back) as usize].1 += s.cost_usd;
                    }
                }
                match kind {
                    "metric" => put("value", json!(money(by_day.last().map_or(0.0, |d| d.1)))),
                    "line" => put("points", json!(by_day.iter().map(|d| d.1).collect::<Vec<_>>())),
                    _ => put("items", json!(by_day.iter().map(|(l, v)| json!({"label": l, "value": v})).collect::<Vec<_>>())),
                }
            }
            b if b.starts_with("factory.") => {
                let m = self.metrics.clone().unwrap_or_default();
                let value = match &b[8..] {
                    "landed" => json!(m.landed),
                    "review" => json!(m.review),
                    "working" => json!(m.working),
                    "ready" => json!(m.ready),
                    "cost_per_landed" => json!(money(m.cost_per_landed)),
                    "waste" => json!(money(m.waste)),
                    "escaped" => json!(m.escaped),
                    _ => Value::Null,
                };
                put("value", value);
                if b == "factory.escaped" && m.escaped > 0 {
                    put("tone", json!("error"));
                }
            }
            _ => {}
        }
        out
    }

    pub(crate) fn make_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let home = std::env::var("HOME").unwrap_or_default();
        self.authoring = false;
        self.teaching = false;
        self.making_view = true;
        self.start_in(home, window, cx);
    }

    pub(crate) fn view_prompt(&mut self, typed: &str) -> Option<String> {
        std::mem::take(&mut self.making_view).then(|| format!("{MAKE}\n\nWhat I want to see: {typed}\n\n{}", tessel_proto::views::format()))
    }

    pub(crate) fn making_view_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.making_view {
            return None;
        }
        let p = self.palette.get();
        Some(
            h_flex()
                .id("view-chip")
                .test_support()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(px(theme::R_TILE))
                .bg(p.accent.opacity(0.12))
                .text_color(p.accent)
                .child(Icon::empty().path("icons/layout-dashboard.svg").size(theme::z(13.)))
                .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).child("New view: say what it should show"))
                .child(Button::new("view-chip-cancel").xsmall().ghost().icon(Icon::empty().path("icons/x.svg")).on_click(cx.listener(|this, _, _, cx| {
                    this.making_view = false;
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    pub(crate) fn views_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let shown = self.view_shown.clone();
        let mut tabs = h_flex().gap_1().flex_wrap().child(self.view_tab("factory", "Factory", shown == "factory", cx));
        for v in &self.saved_views {
            tabs = tabs.child(self.view_tab(&v.id, &v.title, shown == v.id, cx));
        }
        let head = h_flex()
            .px_3()
            .pt_3()
            .pb_2()
            .gap_2()
            .child(div().flex_1().min_w_0().child(tabs))
            .child(Button::new("views-refresh").xsmall().ghost().icon(Icon::empty().path("icons/refresh-cw.svg")).on_click(cx.listener(|this, _, _, cx| {
                this.refresh_views();
                cx.notify();
            })))
            .child(Button::new("views-new").xsmall().outline().label("New View").on_click(cx.listener(|this, _, window, cx| this.make_view(window, cx))));
        let session = self.model.open.as_ref().map(|o| o.id).unwrap_or_else(|| tessel_proto::SessionId(uuid::Uuid::nil()));
        let body: AnyElement = match self.saved_views.iter().find(|v| v.id == shown) {
            Some(v) if !v.problems.is_empty() => v_flex().gap_1().children(v.problems.iter().map(|pr| div().set(Type::Subheadline).text_color(p.failed).child(pr.clone()))).into_any_element(),
            Some(v) => self.element(&format!("view-{}", v.id), session, &v.node, cx),
            None => self.element("view-factory", session, &factory_view(), cx),
        };
        v_flex().id("views").test_support().size_full().child(head).child(div().id("views-body").test_support().flex_1().min_h_0().overflow_y_scroll().px_3().pb_4().child(body)).into_any_element()
    }

    fn view_tab(&self, id: &str, title: &str, on: bool, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let to = id.to_owned();
        div()
            .id(ElementId::from(SharedString::from(format!("view-tab-{id}"))))
            .test_support()
            .px_2()
            .py(px(3.))
            .rounded(px(theme::R_TILE))
            .set(Type::Callout)
            .when(on, |d| d.bg(p.selection).text_color(p.label))
            .when(!on, |d| d.text_color(p.secondary).hover(|d| d.bg(p.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.view_shown = to.clone();
                cx.notify();
            }))
            .child(title.to_owned())
            .into_any_element()
    }
}
