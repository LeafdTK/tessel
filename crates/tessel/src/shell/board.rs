use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
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
        if let Some(p) = &self.board_project {
            return Some(p.clone());
        }
        self.model
            .open_summary()
            .map(|s| s.worktree.as_ref().map_or_else(|| s.cwd.clone(), |w| w.repo.clone()))
            .or_else(|| self.new_folder())
    }

    pub(crate) fn add_task(&mut self, ready: bool, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.task_title.read(cx).value().trim().to_owned();
        if text.is_empty() && self.board_attachments.is_empty() {
            return;
        }
        let Some(repo) = self.board_repo() else {
            self.error = Some("Open a chat in the repository the task is for.".into());
            cx.notify();
            return;
        };
        let attachments = std::mem::take(&mut self.board_attachments);
        let files = std::mem::take(&mut self.board_files);
        let with_files = |mut spec: String| {
            if !files.is_empty() {
                if !spec.is_empty() {
                    spec.push_str("\n\n");
                }
                spec.push_str(&format!("Look at: {}", files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>().join(", ")));
            }
            spec
        };
        let create = |title: String, spec: String, plan: bool, attachments: Vec<Attachment>| Request::TaskCreate { repo: repo.clone(), title, spec, deps: Vec::new(), parent: None, ready, budget_usd: None, plan, from: None, attachments };
        match split_tasks(&text) {
            Split::Issue(issue) => self.cmd(Cmd::Ask { tag: "task", req: Request::TaskFromIssue { repo: repo.clone(), issue, ready } }),
            Split::One(title) => self.cmd(Cmd::Ask { tag: "task", req: create(if title.is_empty() { "Look at the attached".into() } else { title }, with_files(String::new()), false, attachments) }),
            Split::Plan { title, spec } => self.cmd(Cmd::Ask { tag: "task", req: create(title, with_files(spec), true, attachments) }),
            Split::List { context, items } => {
                for (i, item) in items.into_iter().enumerate() {
                    let spec = with_files(if context.is_empty() { String::new() } else { format!("Context for this and the tasks filed with it:\n{context}") });
                    self.cmd(Cmd::Ask { tag: "task", req: create(item, spec, false, if i == 0 { attachments.clone() } else { Vec::new() }) });
                }
            }
        }
        self.task_title.update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub(crate) fn board_paste(&mut self, item: &ClipboardItem, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if let Some(paths) = crate::drops::clipboard_files() {
            self.board_attach_paths(paths, cx);
            return true;
        }
        let mut took = false;
        for entry in item.entries() {
            if let ClipboardEntry::Image(image) = entry {
                let media = match image.format {
                    ImageFormat::Png => "image/png",
                    ImageFormat::Jpeg => "image/jpeg",
                    ImageFormat::Gif => "image/gif",
                    ImageFormat::Webp => "image/webp",
                    _ => continue,
                };
                if image.bytes.len() as u64 > super::composer::MAX_IMAGE {
                    self.error = Some("That image is too big to attach.".into());
                } else {
                    use base64::Engine as _;
                    let n = self.board_attachments.len() + 1;
                    self.board_attachments.push(Attachment { name: format!("Pasted image {n}.{}", media.rsplit('/').next().unwrap_or("png")), media_type: media.into(), data: base64::engine::general_purpose::STANDARD.encode(&image.bytes), element: None });
                }
                took = true;
            }
        }
        let _ = window;
        cx.notify();
        took
    }

    pub(crate) fn board_attach_paths(&mut self, paths: Vec<std::path::PathBuf>, cx: &mut Context<Self>) {
        use base64::Engine as _;
        for path in paths {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
            match super::composer::media_type(&path) {
                Some(media) => match std::fs::read(&path) {
                    Ok(bytes) if bytes.len() as u64 <= super::composer::MAX_IMAGE * 4 => {
                        self.board_attachments.push(Attachment { name, media_type: media.into(), data: base64::engine::general_purpose::STANDARD.encode(bytes), element: None });
                    }
                    Ok(_) => self.error = Some(format!("{name} is too big to attach.")),
                    Err(e) => self.error = Some(format!("Couldn't read {name}: {e}.")),
                },
                None if self.on_host() => self.error = Some(format!("{name} is on this Mac, not on the box: attach pictures and PDFs, or put the file in the project.")),
                None => self.board_files.push(path.to_string_lossy().into_owned()),
            }
        }
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
        let typed = self.task_title.read(cx).value().to_string();
        let (start_label, draft_label) = match split_tasks(&typed) {
            Split::List { items, .. } => (format!("Start {} Tasks", items.len()), format!("Draft {}", items.len())),
            Split::Plan { .. } => ("Plan It".to_owned(), "Draft Plan".to_owned()),
            _ => ("Start".to_owned(), "Draft".to_owned()),
        };
        let (start_label, draft_label) = (start_label.as_str(), draft_label.as_str());
        let mut chips = h_flex().flex_wrap().gap(theme::z(8.));
        for (i, a) in self.board_attachments.iter().enumerate() {
            chips = chips.child(self.board_chip(&p, format!("board-att-{i}"), "image", a.name.clone(), cx.listener(move |this, _, _, cx| {
                if i < this.board_attachments.len() {
                    this.board_attachments.remove(i);
                }
                cx.notify();
            })));
        }
        for (i, f) in self.board_files.iter().enumerate() {
            let name = std::path::Path::new(f).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| f.clone());
            chips = chips.child(self.board_chip(&p, format!("board-file-{i}"), "file-text", name, cx.listener(move |this, _, _, cx| {
                if i < this.board_files.len() {
                    this.board_files.remove(i);
                }
                cx.notify();
            })));
        }
        let has_chips = !self.board_attachments.is_empty() || !self.board_files.is_empty();
        let field = div().w_full().px(theme::z(6.)).text_size(theme::z(16.)).line_height(theme::z(23.)).child({
            let me = cx.entity().downgrade();
            gpui_kit::component::input::Textarea::new(&self.task_title).appearance(false).on_paste(move |item, window, cx| me.update(cx, |this, cx| this.board_paste(item, window, cx)).unwrap_or(false))
        });
        let controls = h_flex()
            .w_full()
            .gap(theme::z(8.))
            .items_center()
            .child(self.board_picker(cx))
            .child(self.rules_button(cx))
            .child(self.issues_button(cx))
            .child(div().flex_1())
            .child(big_button(&p, "task-draft", draft_label, theme::ButtonKind::Secondary, cx.listener(|this, _, window, cx| this.add_task(false, window, cx))))
            .child(big_button(&p, "task-start", start_label, theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| this.add_task(true, window, cx))));
        let composer = v_flex()
            .w_full()
            .pt(theme::z(18.))
            .pb(theme::z(12.))
            .px(theme::z(12.))
            .gap(theme::z(14.))
            .rounded(px(theme::R_CARD + 4.))
            .border_1()
            .border_color(p.separator)
            .when(!glass, |d| d.bg(p.raised).shadow_md())
            .when(glass, |d| d.relative().child(super::glass_spot(theme::R_CARD + 4.)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.board_attach_paths(paths.paths().to_vec(), cx)))
            .child(field)
            .when(has_chips, |d| d.child(div().px(theme::z(6.)).child(chips)))
            .child(controls);
        let mut body = v_flex().w_full().max_w(theme::z(920.)).mx_auto().px(theme::z(28.)).pt(theme::z(24.)).pb(theme::z(40.)).child(composer);
        let mut any = false;
        for (title, states) in SECTIONS {
            let mut here: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state) && self.board_project.as_ref().is_none_or(|p| &t.repo == p)).collect();
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

    pub(crate) fn issues_answer(&mut self, result: Result<Response, String>) {
        self.issues_loading = false;
        if let Some((_, slot)) = &mut self.board_issues {
            *slot = match result {
                Ok(Response::IssueList { issues }) => Ok(issues),
                Ok(_) => Err("GitHub's answer didn't make sense".into()),
                Err(e) => Err(e),
            };
        }
    }

    fn ask_issues(&mut self) {
        let Some(repo) = self.board_repo() else { return };
        self.board_issues = Some((repo.clone(), Ok(Vec::new())));
        self.issues_loading = true;
        self.cmd(Cmd::Ask { tag: "issues", req: Request::IssueList { repo } });
    }

    fn issues_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let me = cx.entity().downgrade();
        let popover = Popover::new("board-issues-popover")
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(
                super::composer::tool_button("board-issues")
                    .flex_shrink_0()
                    .h(theme::z(34.))
                    .px(theme::z(12.))
                    .rounded(theme::z(theme::R_CONTROL))
                    .child(h_flex().gap(theme::z(7.)).items_center().child(Icon::empty().path("icons/circle-dot.svg").size(theme::z(15.))).child(div().text_size(theme::z(13.5)).font_weight(FontWeight::MEDIUM).child("Issues"))),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let mut list = theme::float_list(&p).w(px(420.));
                let Some((repo, got)) = this.board_issues.clone() else { return list.into_any_element() };
                list = list.child(theme::float_label(&p, &format!("Open issues in {}", super::projects::folder_name(&repo))));
                let quiet = |t: String| div().px(theme::z(10.)).py(theme::z(12.)).set(Type::Callout).text_color(p.secondary).child(t);
                match got {
                    Err(e) => list = list.child(quiet(e)),
                    Ok(issues) if issues.is_empty() && this.issues_loading => list = list.child(quiet("Asking GitHub…".into())),
                    Ok(issues) if issues.is_empty() => list = list.child(quiet("No open issues. Label one `tessel` on GitHub and it's started by itself.".into())),
                    Ok(issues) => {
                        let mut rows = v_flex().id("board-issue-list").max_h(px(380.)).overflow_y_scroll();
                        for (i, issue) in issues.into_iter().enumerate() {
                            let filed = this.tasks.iter().any(|t| t.issue.as_deref() == Some(issue.url.as_str()) && t.state != TaskState::Dropped);
                            let (me, popover, url, repo) = (me.clone(), popover.clone(), issue.url.clone(), repo.clone());
                            rows = rows.child(
                                h_flex()
                                    .id(SharedString::from(format!("board-issue-{i}")))
                                    .test_support()
                                    .min_h(theme::z(44.))
                                    .px(theme::z(10.))
                                    .gap(theme::z(10.))
                                    .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                                    .when(!filed, |d| {
                                        d.cursor_pointer().hover(|d| d.bg(p.hover)).on_click(move |_, window, cx| {
                                            popover.update(cx, |st, cx| st.dismiss(window, cx));
                                            let _ = me.update(cx, |this, cx| {
                                                this.cmd(Cmd::Ask { tag: "task", req: Request::TaskFromIssue { repo: repo.clone(), issue: url.clone(), ready: true } });
                                                cx.notify();
                                            });
                                        })
                                    })
                                    .child(div().w(theme::z(44.)).flex_shrink_0().set(Type::Callout).font_features(theme::tabular()).text_color(p.secondary).child(format!("#{}", issue.number)))
                                    .child(div().flex_1().min_w_0().truncate().set(Type::Body).text_color(if filed { p.tertiary } else { p.label }).child(issue.title.clone()))
                                    .when(issue.labels.iter().any(|l| l == "tessel"), |d| d.child(div().px(theme::z(7.)).rounded_full().bg(p.accent.opacity(0.14)).set(Type::Subheadline).text_color(p.accent).child("tessel")))
                                    .child(div().flex_shrink_0().set(Type::Subheadline).text_color(if filed { p.tertiary } else { p.accent }).child(if filed { "On the board" } else { "Start" })),
                            );
                        }
                        list = list.child(rows);
                    }
                }
                list.into_any_element()
            });
        let _ = p;
        div()
            .flex_shrink_0()
            .capture_any_mouse_down(
                cx.listener(|this, _, _, cx| {
                    this.ask_issues();
                    cx.notify();
                }),
            )
            .child(popover)
            .into_any_element()
    }

    pub(crate) fn board_rules_answer(&mut self, result: Result<Response, String>) {
        if let Some((_, slot)) = &mut self.board_rules {
            *slot = Some(match result {
                Ok(Response::Rules { rulebook }) => Ok(rulebook),
                Ok(_) => Err("the answer didn't make sense".into()),
                Err(e) => Err(e),
            });
        }
    }

    pub(crate) fn teach_board(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.board_teach.read(cx).value().trim().to_owned();
        let Some(repo) = self.board_repo() else { return };
        if text.is_empty() {
            return;
        }
        self.board_teach.update(cx, |s, cx| s.set_value("", window, cx));
        self.cmd(Cmd::Create { cwd: repo, worktree: false, prompt: super::proof::teach_prompt(&text), attachments: Vec::new(), model: Some("sonnet".into()) });
        cx.notify();
    }

    fn rules_button(&self, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let popover = Popover::new("board-rules-popover")
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(
                super::composer::tool_button("board-rules")
                    .flex_shrink_0()
                    .h(theme::z(34.))
                    .px(theme::z(12.))
                    .rounded(theme::z(theme::R_CONTROL))
                    .child(h_flex().gap(theme::z(7.)).items_center().child(Icon::empty().path("icons/scale.svg").size(theme::z(15.))).child(div().text_size(theme::z(13.5)).font_weight(FontWeight::MEDIUM).child("Rules"))),
            )
            .content(move |_, _, cx| {
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let mut list = theme::float_list(&p).w(px(460.));
                let Some((repo, got)) = this.board_rules.clone() else { return list.into_any_element() };
                list = list.child(theme::float_label(&p, &format!("House rules for {}", super::projects::folder_name(&repo))));
                let quiet = |t: String| div().px(theme::z(10.)).py(theme::z(10.)).set(Type::Callout).text_color(p.secondary).child(t);
                match got {
                    None => list = list.child(quiet("Reading them…".into())),
                    Some(Err(e)) => list = list.child(quiet(e)),
                    Some(Ok(book)) => {
                        let mut rows = v_flex().id("board-rule-list").max_h(px(300.)).overflow_y_scroll();
                        let mine: Vec<&tessel_proto::proof::Rule> = book.rules.iter().filter(|r| !r.builtin).collect();
                        if mine.is_empty() && book.knowledge.is_empty() {
                            rows = rows.child(quiet("None yet. Every change still gets the built-in checks: no secrets, no skipped tests, the commit style.".into()));
                        }
                        let row = |title: String, kind: &'static str, path: String| {
                            h_flex()
                                .min_h(theme::z(40.))
                                .px(theme::z(10.))
                                .gap(theme::z(10.))
                                .child(div().w(theme::z(70.)).flex_shrink_0().set(Type::Subheadline).text_color(p.secondary).child(kind))
                                .child(v_flex().flex_1().min_w_0().child(div().truncate().set(Type::Body).text_color(p.label).child(title)).child(div().truncate().set(Type::Subheadline).text_color(p.tertiary).child(path)))
                        };
                        for r in mine {
                            let kind = match r.check {
                                tessel_proto::proof::Check::Judge => "Reviewed",
                                tessel_proto::proof::Check::Run { .. } => "Command",
                                tessel_proto::proof::Check::Match { .. } => "Pattern",
                                tessel_proto::proof::Check::Look => "Looks",
                                tessel_proto::proof::Check::Unknown => "Rule",
                            };
                            rows = rows.child(row(r.title.clone(), kind, r.path.clone()));
                        }
                        for k in &book.knowledge {
                            rows = rows.child(row(k.title.clone(), "Knowledge", k.path.clone()));
                        }
                        for sc in &book.scenarios {
                            rows = rows.child(row(sc.title.clone(), if sc.holdout { "Hidden" } else { "Scenario" }, sc.path.clone()));
                        }
                        list = list.child(rows);
                    }
                }
                let target = shell.downgrade();
                let teach = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = target.update(cx, |this, cx| this.teach_board(window, cx));
                };
                list.child(theme::float_sep(&p)).child(
                    v_flex()
                        .p(theme::z(8.))
                        .gap(theme::z(8.))
                        .child(div().h(theme::z(38.)).px(theme::z(12.)).flex().items_center().rounded(px(theme::R_CONTROL)).bg(p.fill).text_size(theme::z(14.)).child(div().w_full().child(Input::new(&this.board_teach).id("board-teach").appearance(false))))
                        .child(
                            h_flex()
                                .gap(theme::z(8.))
                                .child(div().flex_1().set(Type::Subheadline).text_color(p.secondary).child("An agent writes it into .tessel/ as a rule or as knowledge."))
                                .child(big_button(&p, "board-teach-go", "Teach", theme::ButtonKind::Primary, teach)),
                        ),
                )
                .into_any_element()
            });
        div()
            .flex_shrink_0()
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if let Some(repo) = this.board_repo() {
                    this.board_rules = Some((repo.clone(), None));
                    this.cmd(Cmd::Ask { tag: "board.rules", req: Request::RulesFor { repo } });
                    cx.notify();
                }
            }))
            .child(popover)
            .into_any_element()
    }

    fn board_chip(&self, p: &Palette, id: String, icon: &str, name: String, remove: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> AnyElement {
        h_flex()
            .h(theme::z(32.))
            .pl(theme::z(10.))
            .pr(theme::z(4.))
            .gap(theme::z(7.))
            .items_center()
            .rounded_full()
            .bg(p.fill)
            .child(Icon::empty().path(format!("icons/{icon}.svg")).size(theme::z(14.)).text_color(p.secondary))
            .child(div().max_w(theme::z(220.)).truncate().text_size(theme::z(13.)).text_color(p.label).child(name))
            .child(h_flex().id(SharedString::from(id)).size(theme::z(24.)).justify_center().rounded_full().cursor_pointer().text_color(p.secondary).hover(|d| d.bg(p.hover)).on_click(remove).child(Icon::empty().path("icons/x.svg").size(theme::z(12.))))
            .into_any_element()
    }

    fn board_projects(&self) -> Vec<String> {
        let mut all = self.project_list();
        for t in &self.tasks {
            if !all.contains(&t.repo) {
                all.push(t.repo.clone());
            }
        }
        all
    }

    fn board_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let name = self.board_project.as_deref().map(super::projects::folder_name);
        let me = cx.entity().downgrade();
        Popover::new("board-project-popover")
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(
                super::composer::tool_button("board-project")
                    .flex_shrink_0()
                    .h(theme::z(38.))
                    .pl(theme::z(6.))
                    .pr(theme::z(12.))
                    .rounded(theme::z(theme::R_CONTROL + 2.))
                    .bg(p.fill)
                    .child(h_flex().gap(theme::z(9.)).items_center().child(match &name {
                        Some(n) => super::sidebar::project_tile(n, &p).into_any_element(),
                        None => h_flex().size(theme::z(26.)).justify_center().rounded(theme::z(7.)).bg(p.accent.opacity(0.14)).text_color(p.accent).child(Icon::empty().path("icons/layers.svg").size(theme::z(14.))).into_any_element(),
                    })
                    .child(div().max_w(theme::z(180.)).truncate().text_size(theme::z(14.)).font_weight(FontWeight::MEDIUM).text_color(p.label).child(name.clone().unwrap_or_else(|| "All projects".into())))
                    .child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(12.)).text_color(p.secondary))),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let current = this.board_project.clone();
                let mut rows = v_flex().id("board-project-list").max_h(px(340.)).overflow_y_scroll();
                let mut options: Vec<Option<String>> = vec![None];
                options.extend(this.board_projects().into_iter().map(Some));
                for (i, folder) in options.into_iter().enumerate() {
                    let on = current == folder;
                    let name = folder.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "All projects".into());
                    let open = folder.as_ref().map(|f| this.tasks.iter().filter(|t| &t.repo == f && !t.state.finished()).count()).unwrap_or_else(|| this.tasks.iter().filter(|t| !t.state.finished()).count());
                    let (me, popover, pick) = (me.clone(), popover.clone(), folder.clone());
                    rows = rows.child(
                        h_flex()
                            .id(SharedString::from(format!("board-project-{i}")))
                            .test_support()
                            .min_h(theme::z(44.))
                            .px(theme::z(10.))
                            .gap(theme::z(10.))
                            .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                            .cursor_pointer()
                            .when(on, |d| d.bg(p.selection))
                            .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    this.board_project = pick.clone();
                                    cx.notify();
                                });
                            })
                            .child(match &folder {
                                Some(_) => super::sidebar::project_tile(&name, &p).into_any_element(),
                                None => h_flex().size(theme::z(26.)).justify_center().rounded(theme::z(7.)).bg(p.accent.opacity(0.14)).text_color(p.accent).child(Icon::empty().path("icons/layers.svg").size(theme::z(14.))).into_any_element(),
                            })
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().set(Type::Body).text_color(p.label).child(name))
                                    .child(div().truncate().set(Type::Subheadline).text_color(p.tertiary).child(match &folder {
                                        Some(f) => f.clone(),
                                        None => "Every project's tasks".into(),
                                    })),
                            )
                            .when(open > 0, |d| d.child(div().set(Type::Subheadline).font_features(theme::tabular()).text_color(p.secondary).child(open.to_string())))
                            .when(on, |d| d.child(Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.accent))),
                    );
                }
                theme::float_list(&p).w(px(340.)).child(theme::float_label(&p, "Tasks go to")).child(rows).into_any_element()
            })
            .into_any_element()
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
        if let Some(url) = t.issue.clone() {
            let label = url.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "Issue".to_owned(), |n| format!("Issue #{n}"));
            actions = actions.child(big_button(&p, format!("task-issue-{id}"), &label, theme::ButtonKind::Ghost, cx.listener(move |_, _, _, cx| {
                cx.stop_propagation();
                cx.open_url(&url);
            })));
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

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Split {
    Issue(String),
    One(String),
    Plan { title: String, spec: String },
    List { context: String, items: Vec<String> },
}

#[cfg(test)]
pub(crate) fn split_for_test(text: &str) -> Split {
    split_tasks(text)
}

fn bullet(line: &str) -> Option<String> {
    let t = line.trim_start();
    let rest = ["- [ ] ", "- [x] ", "- ", "* ", "• "].iter().find_map(|m| t.strip_prefix(m)).or_else(|| {
        let digits = t.chars().take_while(|c| c.is_ascii_digit()).count();
        (digits > 0 && digits < 4).then(|| t[digits..].strip_prefix(". ").or_else(|| t[digits..].strip_prefix(") "))).flatten()
    })?;
    let item = rest.trim();
    (!item.is_empty()).then(|| item.to_owned())
}

pub(crate) fn split_tasks(text: &str) -> Split {
    let text = text.trim();
    if is_issue(text) {
        return Split::Issue(text.to_owned());
    }
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let items: Vec<String> = lines.iter().filter_map(|l| bullet(l)).collect();
    if items.len() >= 2 {
        let first = lines.iter().position(|l| bullet(l).is_some()).unwrap_or(0);
        return Split::List { context: lines[..first].join("\n"), items };
    }
    if lines.len() >= 2 || text.chars().count() > 200 {
        let head = lines.first().copied().unwrap_or(text).trim().trim_start_matches('#').trim();
        let cut = head.find(". ").map_or(head, |i| &head[..i]);
        let title: String = cut.chars().take(80).collect();
        return Split::Plan { title, spec: text.to_owned() };
    }
    Split::One(text.to_owned())
}

fn is_issue(text: &str) -> bool {
    let t = text.trim();
    let number = |n: &str| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit());
    t.strip_prefix('#').is_some_and(number) || (t.contains("github.com/") && t.split('/').collect::<Vec<_>>().windows(2).any(|w| w[0] == "issues" && number(w[1].split(['?', '#']).next().unwrap_or(""))))
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
