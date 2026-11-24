use super::*;
use gpui_kit::component::Icon;
use tessel_proto::task::{Task, TaskAct, TaskState};
use tessel_proto::{Request, Response};

const SECTIONS: [(&str, &[TaskState]); 5] = [
    ("Review", &[TaskState::Review]),
    ("Working", &[TaskState::Running, TaskState::Proving, TaskState::Landing, TaskState::Checking, TaskState::Planned]),
    ("Up Next", &[TaskState::Ready]),
    ("Drafts", &[TaskState::Draft]),
    ("Landed", &[TaskState::Landed]),
];

impl Shell {
    pub(crate) fn task_updated(&mut self, task: Task) {
        match self.tasks.iter_mut().find(|t| t.id == task.id) {
            Some(t) => *t = task,
            None => self.tasks.push(task),
        }
    }

    pub(crate) fn tasks_answer(&mut self, result: Result<Response, String>) {
        if let Ok(Response::Tasks { tasks }) = result {
            self.tasks = tasks;
        }
    }

    pub(crate) fn task_answer(&mut self, result: Result<Response, String>) {
        match result {
            Ok(Response::Task { task }) => self.task_updated(task),
            Ok(_) => {}
            Err(e) => self.error = Some(e),
        }
    }

    fn board_repo(&self) -> Option<String> {
        self.model
            .open_summary()
            .map(|s| s.worktree.as_ref().map_or_else(|| s.cwd.clone(), |w| w.repo.clone()))
            .or_else(|| self.new_folder())
    }

    pub(crate) fn add_task(&mut self, ready: bool, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.task_title.read(cx).value().trim().to_owned();
        if title.is_empty() {
            return;
        }
        let Some(repo) = self.board_repo() else {
            self.error = Some("Open a chat in the repository the task is for.".into());
            cx.notify();
            return;
        };
        self.cmd(Cmd::Ask { tag: "task", req: Request::TaskCreate { repo, title, spec: String::new(), deps: Vec::new(), parent: None, ready, budget_usd: None, plan: false, from: None } });
        self.task_title.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub(crate) fn act_task(&mut self, id: &str, act: TaskAct, cx: &mut Context<Self>) {
        cx.stop_propagation();
        self.cmd(Cmd::Ask { tag: "task", req: Request::TaskAct { id: id.to_owned(), act } });
        cx.notify();
    }

    pub(crate) fn change_task(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.changing_task.take() else { return };
        let text = self.task_change.read(cx).value().trim().to_owned();
        self.task_change.update(cx, |s, cx| s.set_value("", window, cx));
        if !text.is_empty() {
            self.act_task(&id, TaskAct::Change { text }, cx);
        }
        cx.notify();
    }

    pub(crate) fn change_task_and_teach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.task_change.read(cx).value().trim().to_owned();
        let repo = self.changing_task.as_ref().and_then(|id| self.tasks.iter().find(|t| &t.id == id)).map(|t| t.repo.clone());
        self.change_task(window, cx);
        if let (false, Some(repo)) = (text.is_empty(), repo) {
            self.teach_from_review(repo, text, cx);
        }
    }

    fn task_cost(&self, t: &Task) -> f64 {
        t.proof_cost_usd + t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).map_or(0.0, |s| s.cost_usd)
    }

    pub(crate) fn board_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let glass = crate::glass::liquid();
        let composer = h_flex()
            .w_full()
            .min_h(theme::z(64.))
            .pl(theme::z(18.))
            .pr(theme::z(12.))
            .py(theme::z(12.))
            .gap(theme::z(14.))
            .items_center()
            .rounded(px(theme::R_CARD + 2.))
            .border_1()
            .border_color(p.separator)
            .when(!glass, |d| d.bg(p.raised).shadow_md())
            .when(glass, |d| d.relative().child(super::glass_spot(theme::R_CARD + 2.)))
            .child(
                h_flex()
                    .size(theme::z(30.))
                    .flex_shrink_0()
                    .justify_center()
                    .rounded_full()
                    .bg(p.accent.opacity(0.12))
                    .text_color(p.accent)
                    .child(Icon::empty().path("icons/plus.svg").size(theme::z(16.))),
            )
            .child(div().flex_1().min_w_0().text_size(theme::z(15.)).child(Input::new(&self.task_title).id("task-title").appearance(false)))
            .child(big_button(&p, "task-draft", "Draft", theme::ButtonKind::Secondary, cx.listener(|this, _, window, cx| this.add_task(false, window, cx))))
            .child(big_button(&p, "task-start", "Start", theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| this.add_task(true, window, cx))));
        let mut body = v_flex().w_full().max_w(theme::z(920.)).mx_auto().px(theme::z(28.)).pt(theme::z(24.)).pb(theme::z(40.)).child(composer);
        let mut any = false;
        for (title, states) in SECTIONS {
            let mut here: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state)).collect();
            if here.is_empty() {
                continue;
            }
            any = true;
            here.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
            let total = here.len();
            if title == "Landed" {
                here.truncate(8);
            }
            body = body.child(
                h_flex()
                    .pt(theme::z(32.))
                    .pb(theme::z(12.))
                    .gap(theme::z(10.))
                    .items_center()
                    .child(div().text_size(theme::z(17.)).font_weight(FontWeight::SEMIBOLD).text_color(p.label).child(title))
                    .child(
                        div()
                            .h(theme::z(22.))
                            .px(theme::z(9.))
                            .flex()
                            .items_center()
                            .rounded_full()
                            .bg(p.fill)
                            .text_size(theme::z(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .font_features(theme::tabular())
                            .text_color(p.secondary)
                            .child(total.to_string()),
                    ),
            );
            let mut list = v_flex().gap(theme::z(10.));
            for t in here {
                list = list.child(self.task_card(t, cx));
            }
            body = body.child(list);
        }
        if !any {
            body = body.child(empty_board(&p));
        }
        v_flex().id("board").size_full().overflow_y_scroll().child(body).into_any_element()
    }

    fn task_card(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let id = t.id.clone();
        let session = t.session;
        let summary = session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s));
        let working = matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking);
        let status = match t.state {
            TaskState::Proving => "Proving".to_owned(),
            TaskState::Planned => {
                let kids: Vec<&Task> = self.tasks.iter().filter(|x| x.parent.as_deref() == Some(t.id.as_str())).collect();
                let done = kids.iter().filter(|k| k.state == TaskState::Landed).count();
                format!("Split into {}: {done} landed", kids.len())
            }
            TaskState::Running if t.plan => "Planning".to_owned(),
            TaskState::Landing => "Landing".to_owned(),
            TaskState::Checking => t.notes.iter().rev().find(|n| n.by == "tessel").map(|n| super::sentence(&n.text)).filter(|n| n.starts_with("Merging")).unwrap_or_else(|| "Waiting for its checks".into()),
            TaskState::Running => summary.and_then(|s| s.activity.clone()).map(|a| super::sentence(&a)).unwrap_or_else(|| "Working".into()),
            TaskState::Review => t
                .notes
                .last()
                .map(|n| n.text.as_str())
                .filter(|n| !["proven", "failed", "couldn't be proved"].iter().any(|v| n.starts_with(v)) || n.contains(": "))
                .map(super::sentence)
                .unwrap_or_else(|| "Ready for you".into()),
            TaskState::Landed => t.landed.clone().map(|l| format!("Landed {l}")).unwrap_or_else(|| "Landed".into()),
            TaskState::Ready if !t.deps.is_empty() => {
                let waiting: Vec<String> = t.deps.iter().filter_map(|d| self.tasks.iter().find(|x| &x.id == d && x.state != TaskState::Landed)).map(|x| format!("“{}”", x.title.chars().take(40).collect::<String>())).collect();
                if waiting.is_empty() { "Up next".into() } else { format!("After {}", waiting.join(", ")) }
            }
            TaskState::Ready => "Up next".into(),
            TaskState::Draft => "Draft".into(),
            _ => String::new(),
        };
        let cost = self.task_cost(t);
        let repo = t.repo.rsplit('/').next().unwrap_or(&t.repo).to_owned();
        let mut meta = vec![repo];
        if cost >= 0.005 {
            meta.push(format!("${cost:.2}"));
        }
        meta.push(t.id.clone());
        let mut actions = h_flex().flex_shrink_0().gap(theme::z(8.)).items_center();
        match t.state {
            TaskState::Draft => {
                let (i, j) = (id.clone(), id.clone());
                actions = actions
                    .child(big_button(&p, format!("task-plan-{id}"), "Plan", theme::ButtonKind::Secondary, cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Plan, cx))))
                    .child(big_button(&p, format!("task-ready-{id}"), "Start", theme::ButtonKind::Primary, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Ready, cx))));
            }
            TaskState::Review => {
                let (i, j) = (id.clone(), id.clone());
                actions = actions
                    .child(big_button(&p, format!("task-change-{id}"), "Change", theme::ButtonKind::Secondary, cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.changing_task = Some(j.clone());
                        this.task_change.update(cx, |s, cx| s.focus(window, cx));
                        cx.notify();
                    })))
                    .child(big_button(&p, format!("task-land-{id}"), "Land", theme::ButtonKind::Primary, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))));
            }
            _ => {}
        }
        if let Some(url) = t.pr.clone() {
            actions = actions.child(big_button(&p, format!("task-pr-{id}"), "Pull Request", theme::ButtonKind::Secondary, cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url);
            })));
        }
        if !t.state.finished() && t.state != TaskState::Landing {
            let i = id.clone();
            actions = actions.child(
                h_flex()
                    .id(ElementId::from(SharedString::from(format!("task-drop-{id}"))))
                    .test_support()
                    .size(theme::z(34.))
                    .justify_center()
                    .rounded_full()
                    .text_color(p.secondary)
                    .cursor_pointer()
                    .hover(|d| d.bg(p.hover).text_color(p.label))
                    .on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Drop, cx)))
                    .child(Icon::empty().path("icons/x.svg").size(theme::z(15.))),
            );
        }
        let verdict = summary.and_then(|s| s.proof.as_ref()).filter(|_| !t.state.finished()).map(|proof| {
            let tone = match proof.verdict {
                tessel_proto::proof::Verdict::Failed | tessel_proto::proof::Verdict::Broken => p.failed,
                tessel_proto::proof::Verdict::Concerns => p.blocked,
                tessel_proto::proof::Verdict::Proven => p.finished,
                _ => p.secondary,
            };
            let word = super::sentence(&super::proof::verdict_word(proof));
            h_flex()
                .flex_shrink_0()
                .h(theme::z(24.))
                .px(theme::z(10.))
                .gap(theme::z(6.))
                .items_center()
                .rounded_full()
                .bg(tone.opacity(0.13))
                .child(div().size(theme::z(6.)).rounded_full().bg(tone))
                .child(div().text_size(theme::z(12.)).font_weight(FontWeight::MEDIUM).text_color(if p.dark { tone } else { tone.blend(p.label.opacity(0.35)) }).child(word))
        });
        let mark = match t.state {
            _ if working => super::live_dot(p.blocked, &p).into_any_element(),
            TaskState::Landed => h_flex().size(theme::z(18.)).flex_shrink_0().justify_center().text_color(p.finished).child(Icon::empty().path("icons/circle-check.svg").size(theme::z(16.))).into_any_element(),
            TaskState::Review => state_dot(p.accent, false, &p),
            TaskState::Draft => state_dot(p.tertiary, true, &p),
            _ => state_dot(p.secondary, false, &p),
        };
        let changing = self.changing_task.as_deref() == Some(id.as_str());
        let glass = crate::glass::liquid();
        v_flex()
            .id(ElementId::from(SharedString::from(format!("task-{id}"))))
            .test_support()
            .aria_label(SharedString::from(t.title.clone()))
            .w_full()
            .px(theme::z(20.))
            .py(theme::z(16.))
            .gap(theme::z(12.))
            .rounded(px(theme::R_CARD))
            .border_1()
            .border_color(p.separator.opacity(0.7))
            .when(!glass, |d| d.bg(p.raised).shadow_sm())
            .when(glass, |d| d.bg(p.fill))
            .when(session.is_some(), |d| d.cursor_pointer().hover(|d| d.border_color(p.separator.opacity(1.4))))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(s) = session {
                    this.switch_open(s, false, cx);
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .gap(theme::z(14.))
                    .items_center()
                    .child(mark)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(theme::z(4.))
                            .child(
                                h_flex()
                                    .gap(theme::z(10.))
                                    .items_center()
                                    .child(div().min_w_0().text_size(theme::z(15.)).line_height(theme::z(20.)).font_weight(FontWeight::MEDIUM).text_color(p.label).truncate().child(t.title.clone()))
                                    .children(verdict),
                            )
                            .child(
                                h_flex()
                                    .gap(theme::z(8.))
                                    .min_w_0()
                                    .text_size(theme::z(13.))
                                    .line_height(theme::z(18.))
                                    .when(!status.is_empty(), |d| d.child(div().min_w_0().truncate().text_color(p.secondary).child(status)).child(div().flex_shrink_0().text_color(p.tertiary).child("·")))
                                    .child(div().flex_shrink_0().font_features(theme::tabular()).text_color(p.tertiary).child(meta.join("  ·  "))),
                            ),
                    )
                    .child(actions),
            )
            .when(changing, |d| {
                d.child(
                    h_flex()
                        .gap(theme::z(8.))
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .h(theme::z(38.))
                                .px(theme::z(12.))
                                .flex()
                                .items_center()
                                .rounded(px(theme::R_CONTROL))
                                .bg(p.fill)
                                .text_size(theme::z(14.))
                                .child(div().w_full().child(Input::new(&self.task_change).id("task-change").appearance(false))),
                        )
                        .child(big_button(&p, "task-change-rule", "Send + Rule", theme::ButtonKind::Ghost, cx.listener(|this, _, window, cx| this.change_task_and_teach(window, cx))))
                        .child(big_button(&p, "task-change-send", "Send", theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| this.change_task(window, cx)))),
                )
            })
            .into_any_element()
    }
}

fn big_button(p: &Palette, id: impl Into<SharedString>, label: &str, kind: theme::ButtonKind, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    theme::button(p, id, label.to_owned(), None, kind).h(theme::z(34.)).px(theme::z(16.)).text_size(theme::z(13.5)).rounded(theme::z(theme::R_CONTROL)).on_click(on_click).test_support()
}

fn state_dot(color: Hsla, hollow: bool, p: &Palette) -> AnyElement {
    let _ = p;
    h_flex()
        .size(theme::z(18.))
        .flex_shrink_0()
        .justify_center()
        .child(div().size(theme::z(10.)).rounded_full().when(hollow, |d| d.border_2().border_color(color)).when(!hollow, |d| d.bg(color)))
        .into_any_element()
}

fn empty_board(p: &Palette) -> Div {
    let step = |icon: &str, title: &str, body: &str| {
        v_flex()
            .flex_1()
            .min_w_0()
            .p(theme::z(18.))
            .gap(theme::z(8.))
            .rounded(px(theme::R_CARD))
            .bg(p.fill)
            .child(h_flex().size(theme::z(32.)).justify_center().rounded(theme::z(9.)).bg(p.raised).text_color(p.accent).child(Icon::empty().path(format!("icons/{icon}.svg")).size(theme::z(17.))))
            .child(div().pt(theme::z(4.)).text_size(theme::z(14.)).font_weight(FontWeight::SEMIBOLD).text_color(p.label).child(title.to_owned()))
            .child(div().text_size(theme::z(13.)).line_height(theme::z(18.)).text_color(p.secondary).child(body.to_owned()))
    };
    v_flex()
        .w_full()
        .pt(theme::z(56.))
        .items_center()
        .gap(theme::z(14.))
        .child(
            h_flex()
                .size(theme::z(60.))
                .justify_center()
                .rounded(theme::z(18.))
                .bg(p.accent.opacity(0.12))
                .text_color(p.accent)
                .child(Icon::empty().path("icons/square-kanban.svg").size(theme::z(28.))),
        )
        .child(div().pt(theme::z(6.)).text_size(theme::z(22.)).font_weight(FontWeight::SEMIBOLD).text_color(p.label).child("Nothing on the board yet"))
        .child(
            div()
                .max_w(theme::z(460.))
                .text_center()
                .text_size(theme::z(14.))
                .line_height(theme::z(21.))
                .text_color(p.secondary)
                .child("Describe a change above. An agent makes it in a workspace of its own, it's proved against your checks and rules, and it comes back to you to land."),
        )
        .child(
            h_flex()
                .w_full()
                .pt(theme::z(26.))
                .gap(theme::z(12.))
                .items_stretch()
                .child(step("pencil-line", "Describe", "One change per task. Start sends it to an agent now; Draft keeps it here."))
                .child(step("shield-check", "Proved", "Tests, house rules, screenshots and reviewers check it before you see it."))
                .child(step("git-merge", "Land", "Land it in one commit, ask for a change, or let small safe ones land by themselves.")),
        )
}
