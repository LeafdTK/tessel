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
        let mut col = v_flex().id("board").size_full().overflow_y_scroll().pb_4().child(
            h_flex()
                .px_4()
                .pt_4()
                .pb_3()
                .gap_2()
                .child(div().flex_1().child(Input::new(&self.task_title).id("task-title")))
                .child(Button::new("task-add").small().outline().label("Add").on_click(cx.listener(|this, _, window, cx| this.add_task(false, window, cx)))),
        );
        let mut any = false;
        for (title, states) in SECTIONS {
            let mut here: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state)).collect();
            if here.is_empty() {
                continue;
            }
            any = true;
            here.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
            if title == "Landed" {
                here.truncate(8);
            }
            col = col.child(self.section(title));
            for t in here {
                col = col.child(self.task_card(t, cx));
            }
        }
        if !any {
            col = col.child(div().px_3().py_2().set(Type::Subheadline).text_color(p.secondary).child("Add a task, then mark it ready: an agent picks it up in a workspace of its own."));
        }
        col.into_any_element()
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
            TaskState::Running => summary.and_then(|s| s.activity.clone()).unwrap_or_else(|| "Working".into()),
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
            _ => String::new(),
        };
        let cost = self.task_cost(t);
        let repo = t.repo.rsplit('/').next().unwrap_or(&t.repo).to_owned();
        let button = |name: &str| Button::new(ElementId::from(SharedString::from(format!("task-{name}-{id}")))).small();
        let mut actions = h_flex().gap_2();
        match t.state {
            TaskState::Draft => {
                let (i, j) = (id.clone(), id.clone());
                actions = actions
                    .child(button("plan").ghost().label("Plan").on_click(cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Plan, cx))))
                    .child(button("ready").outline().label("Ready").on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Ready, cx))));
            }
            TaskState::Review => {
                let (i, j) = (id.clone(), id.clone());
                actions = actions
                    .child(button("land").primary().label("Land").on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))))
                    .child(button("change").outline().label("Change").on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.changing_task = Some(j.clone());
                        this.task_change.update(cx, |s, cx| s.focus(window, cx));
                        cx.notify();
                    })));
            }
            _ => {}
        }
        if let Some(url) = t.pr.clone() {
            actions = actions.child(button("pr").ghost().label("Pull Request").on_click(cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url);
            })));
        }
        if !t.state.finished() && t.state != TaskState::Landing {
            let i = id.clone();
            actions = actions.child(button("drop").ghost().icon(Icon::empty().path("icons/x.svg")).on_click(cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Drop, cx))));
        }
        let changing = self.changing_task.as_deref() == Some(id.as_str());
        v_flex()
            .id(ElementId::from(SharedString::from(format!("task-{id}"))))
            .test_support()
            .aria_label(SharedString::from(t.title.clone()))
            .mx_3()
            .mb_2()
            .px_4()
            .py_3()
            .gap(px(6.))
            .rounded(px(theme::R_CARD))
            .border_1()
            .border_color(p.separator)
            .when(session.is_some(), |d| d.hover(|d| d.bg(p.hover)))
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(s) = session {
                    this.switch_open(s, false, cx);
                    cx.notify();
                }
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .when(working, |d| d.child(super::live_dot(p.blocked, &p)))
                    .child(div().flex_1().min_w_0().set(Type::Callout).text_color(p.label).truncate().child(t.title.clone()))
                    .when_some(summary.and_then(|s| s.proof.as_ref()), |d, proof| d.child(div().flex_shrink_0().set(Type::Subheadline).text_color(match proof.verdict {
                        tessel_proto::proof::Verdict::Failed | tessel_proto::proof::Verdict::Broken => p.failed,
                        tessel_proto::proof::Verdict::Concerns => p.blocked,
                        _ => p.tertiary,
                    }).child(super::proof::verdict_word(proof))))
                    .child(actions),
            )
            .child(
                h_flex()
                    .gap_2()
                    .set(Type::Subheadline)
                    .text_color(p.tertiary)
                    .child(format!("{} · {repo}", t.id))
                    .when(cost >= 0.005, |d| d.child(div().font_features(theme::tabular()).child(format!("${cost:.2}"))))
                    .when(!status.is_empty(), |d| d.child(div().flex_1().min_w_0().truncate().text_color(p.secondary).child(status))),
            )
            .when(changing, |d| {
                d.child(
                    h_flex()
                        .pt_1()
                        .gap_2()
                        .child(div().flex_1().child(Input::new(&self.task_change).id("task-change").small()))
                        .child(Button::new("task-change-rule").small().ghost().label("Send + Rule").on_click(cx.listener(|this, _, window, cx| this.change_task_and_teach(window, cx))))
                        .child(Button::new("task-change-send").small().outline().label("Send").on_click(cx.listener(|this, _, window, cx| this.change_task(window, cx)))),
                )
            })
            .into_any_element()
    }
}
