use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use tessel_proto::task::{Task, TaskAct, TaskState};
use tessel_proto::{Request, Response};

const GUTTER: f32 = 20.;
const ROUND: f32 = theme::R_CONTROL + GUTTER / theme::SQUIRCLE;

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
        let land = self.board_land.clone().map(|l| if l == "branch" { format!("branch:{}", self.board_branch.read(cx).value().trim()) } else { l }).filter(|l| l != "branch:");
        let create = |title: String, spec: String, plan: bool, attachments: Vec<Attachment>| Request::TaskCreate { repo: repo.clone(), title, spec, deps: Vec::new(), parent: None, ready, budget_usd: None, plan, from: None, attachments, land: land.clone() };
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

    pub(crate) fn task_cost(&self, t: &Task) -> f64 {
        t.proof_cost_usd + t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).map_or(0.0, |s| s.cost_usd)
    }


    pub(crate) fn scope_picker(&self, size: f32, cx: &mut Context<Self>) -> AnyElement {
        let name = self.board_project.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "All projects".into());
        let me = cx.entity().downgrade();
        Popover::new("board-project-popover").rounded(crate::theme::z(crate::theme::R_FLOAT))
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(
                super::composer::tool_button("board-project")
                    .h(theme::z(44.))
                    .px(theme::z(6.))
                    .ml(theme::z(-6.))
                    .child(h_flex().gap(theme::z(8.)).items_center().child(div().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(size)).truncate().child(name)).child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(16.)))),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let current = this.board_project.clone();
                let mut rows = v_flex().id("board-project-list").max_h(px(360.)).overflow_y_scroll();
                let mut options: Vec<Option<String>> = vec![None];
                options.extend(this.board_projects().into_iter().map(Some));
                for (i, folder) in options.into_iter().enumerate() {
                    let on = current == folder;
                    let name = folder.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "All projects".into());
                    let (me, popover, pick) = (me.clone(), popover.clone(), folder.clone());
                    rows = rows.child(
                        theme::float_item(&p, format!("board-project-{i}"), None, name, None, on)
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    this.board_project = pick.clone();
                                    this.board_issues = None;
                                    this.board_rules = None;
                                    cx.notify();
                                });
                            })
                            .test_support(),
                    );
                }
                theme::float_list(&p).w(px(300.)).child(rows).into_any_element()
            })
            .into_any_element()
    }

    fn when_done(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let default = self.daemon_settings.as_ref().map_or_else(|| "main".to_owned(), |d| d.land.clone());
        let chosen = self.board_land.clone().unwrap_or(default.clone());
        let label = if chosen == "branch" { "Merge into a branch".to_owned() } else { tessel_proto::task::land_word(&chosen) };
        let me = cx.entity().downgrade();
        let picker = Popover::new("board-land-popover").rounded(crate::theme::z(crate::theme::R_FLOAT))
            .p_0()
            .anchor(Anchor::BottomLeft)
            .trigger(
                super::composer::tool_button("board-land")
                    .h(theme::z(36.))
                    .px(theme::z(12.))
                    .bg(p.fill)
                    .child(h_flex().gap(theme::z(8.)).items_center().child(div().text_size(theme::z(14.)).text_color(p.label).child(label)).child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(13.)).text_color(p.secondary))),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let current = this.board_land.clone().unwrap_or(default.clone());
                let mut rows = v_flex();
                for (i, (value, label)) in [("main", "Land on main"), ("pr", "Open a pull request"), ("branch", "Merge into a branch"), ("keep", "Keep the branch")].into_iter().enumerate() {
                    let on = current == value;
                    let (me, popover) = (me.clone(), popover.clone());
                    rows = rows.child(
                        theme::float_item(&p, format!("board-land-option-{i}"), None, label, None, on)
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    this.board_land = Some(value.to_owned());
                                    cx.notify();
                                });
                            })
                            .test_support(),
                    );
                }
                theme::float_list(&p).w(px(240.)).child(rows).into_any_element()
            });
        h_flex()
            .gap(theme::z(8.))
            .items_center()
            .child(picker)
            .when(chosen == "branch", |d| {
                d.child(div().w(theme::z(180.)).h(theme::z(36.)).px(theme::z(10.)).flex().items_center().rounded(theme::z(theme::R_CONTROL)).border_1().border_color(p.separator).text_size(theme::z(14.)).child(div().w_full().child(Input::new(&self.board_branch).id("board-branch").appearance(false))))
            })
            .into_any_element()
    }

    pub(crate) fn task_composer(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let glass = crate::glass::liquid();
        let typed = self.task_title.read(cx).value().to_string();
        let (start_label, draft_label) = match split_tasks(&typed) {
            Split::List { items, .. } => (format!("Start {} Tasks", items.len()), format!("Save {} as Drafts", items.len())),
            Split::Plan { .. } => ("Plan It".to_owned(), "Save as Draft".to_owned()),
            _ => ("Start".to_owned(), "Save as Draft".to_owned()),
        };
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
        let composer = v_flex()
            .w_full()
            .p(theme::z(GUTTER))
            .gap(theme::z(16.))
            .rounded(theme::z(ROUND))
            .border_1()
            .border_color(p.separator)
            .when(!glass, |d| d.bg(p.raised).shadow_md())
            .when(glass, |d| d.relative().child(super::glass_spot(ROUND)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.board_attach_paths(paths.paths().to_vec(), cx)))
            .child(div().w_full().ml(theme::z(-12.)).text_size(theme::z(17.)).line_height(theme::z(24.)).child({
                let me = cx.entity().downgrade();
                gpui_kit::component::input::Textarea::new(&self.task_title).appearance(false).on_paste(move |item, window, cx| me.update(cx, |this, cx| this.board_paste(item, window, cx)).unwrap_or(false))
            }))
            .when(has_chips, |d| d.child(chips))
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap(theme::z(10.))
                    .items_center()
                    .child(div().ml(theme::z(-2.)).child(self.when_done(cx)))
                    .child(div().flex_1())
                    .child(big_button(&p, "task-draft", &draft_label, theme::ButtonKind::Secondary, cx.listener(|this, _, window, cx| this.add_task(false, window, cx))))
                    .child(big_button(&p, "task-start", &start_label, theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| this.add_task(true, window, cx)))),
            );
        composer.into_any_element()
    }

    pub(crate) fn doing(&self, t: &Task) -> String {
        match t.state {
            TaskState::Running if t.plan => "Planning".into(),
            TaskState::Running => t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).and_then(|s| s.activity.clone()).map(|a| super::sentence(&a)).unwrap_or_else(|| "Building".into()),
            TaskState::Proving if t.notes.last().is_some_and(|n| n.text == "in final review") => "Final review".into(),
            TaskState::Proving => "Checking".into(),
            TaskState::Landing => "Landing".into(),
            TaskState::Checking => "Waiting for CI".into(),
            TaskState::Planned => {
                let kids: Vec<&Task> = self.tasks.iter().filter(|x| x.parent.as_deref() == Some(t.id.as_str())).collect();
                format!("{} of {} done", kids.iter().filter(|k| k.state == TaskState::Landed).count(), kids.len())
            }
            TaskState::Ready if !t.deps.is_empty() => "Waiting for another task".into(),
            TaskState::Ready => "Queued".into(),
            _ => String::new(),
        }
    }

    pub(crate) fn outcome(&self, t: &Task) -> String {
        match t.state {
            TaskState::Review => t.ask.clone().unwrap_or_else(|| t.notes.iter().rev().find(|n| n.by == "tessel").map(|n| super::sentence(&n.text)).unwrap_or_default()),
            TaskState::Landed => {
                let to = t.landed.clone().unwrap_or_default();
                if to.starts_with("http") {
                    "Pull request merged".into()
                } else if to.contains('/') {
                    format!("Kept on {to}")
                } else {
                    let into = t.notes.iter().rev().find_map(|n| n.text.strip_prefix("Merged into ").map(|b| b.trim_end_matches('.').to_owned())).unwrap_or_else(|| "main".into());
                    format!("Merged into {into} as {to}")
                }
            }
            TaskState::Draft => String::new(),
            _ => self.doing(t),
        }
    }

    pub(crate) fn issues_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let quiet = |t: String| div().py(theme::z(12.)).text_size(theme::z(15.)).text_color(p.secondary).child(t).into_any_element();
        let Some((repo, got)) = self.board_issues.clone() else { return quiet("Pick a project to see its issues.".into()) };
        let mut col = v_flex().w_full().gap(theme::z(10.));
        match got {
            Err(e) => col = col.child(quiet(e)),
            Ok(issues) if issues.is_empty() && self.issues_loading => col = col.child(quiet("Asking GitHub…".into())),
            Ok(issues) if issues.is_empty() => col = col.child(quiet("No open issues.".into())),
            Ok(issues) => {
                for (i, issue) in issues.into_iter().enumerate() {
                    let filed = self.tasks.iter().any(|t| t.issue.as_deref() == Some(issue.url.as_str()) && t.state != TaskState::Dropped);
                    let (url, repo, open) = (issue.url.clone(), repo.clone(), issue.url.clone());
                    col = col.child(
                        h_flex()
                            .id(SharedString::from(format!("board-issue-row-{i}")))
                            .w_full()
                            .pl(theme::z(GUTTER))
                            .pr(theme::z(14.))
                            .py(theme::z(14.))
                            .gap(theme::z(16.))
                            .items_center()
                            .rounded(theme::z(theme::R_CONTROL + 14. / theme::SQUIRCLE))
                            .bg(p.raised)
                            .border_1()
                            .border_color(p.separator)
                            .child(div().w(theme::z(56.)).flex_shrink_0().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(17.)).font_features(theme::tabular()).text_color(p.tertiary).child(format!("{}", issue.number)))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().truncate().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::SEMIBOLD).text_size(theme::z(16.)).text_color(p.label).child(issue.title.clone()))
                                    .when(!issue.labels.is_empty(), |d| d.child(div().truncate().text_size(theme::z(13.)).text_color(p.secondary).child(issue.labels.join(", ")))),
                            )
                            .child(big_button(&p, format!("board-issue-open-{i}"), "View", theme::ButtonKind::Ghost, cx.listener(move |_, _, _, cx| cx.open_url(&open))))
                            .child(if filed {
                                div().w(theme::z(84.)).text_center().text_size(theme::z(14.)).text_color(p.secondary).child("On the board").into_any_element()
                            } else {
                                big_button(&p, format!("board-issue-{i}"), "Start", theme::ButtonKind::Primary, cx.listener(move |this, _, _, cx| {
                                    this.cmd(Cmd::Ask { tag: "task", req: Request::TaskFromIssue { repo: repo.clone(), issue: url.clone(), ready: true } });
                                    cx.notify();
                                }))
                                .into_any_element()
                            }),
                    );
                }
            }
        }
        col.into_any_element()
    }

    pub(crate) fn ask_rules(&mut self) {
        if let Some(repo) = self.board_repo() {
            self.board_rules = Some((repo.clone(), None));
            self.cmd(Cmd::Ask { tag: "board.rules", req: Request::RulesFor { repo } });
        }
    }

    pub(crate) fn rules_tab(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let quiet = |t: String| div().py(theme::z(12.)).text_size(theme::z(15.)).text_color(p.secondary).child(t).into_any_element();
        let teach = v_flex()
            .w_full()
            .p(theme::z(20.))
            .gap(theme::z(16.))
            .rounded(theme::z(ROUND))
            .bg(p.raised)
            .border_1()
            .border_color(p.separator)
            .child(div().w_full().text_size(theme::z(17.)).child(Input::new(&self.board_teach).id("board-teach").appearance(false)))
            .child(h_flex().w_full().child(div().flex_1()).child(big_button(&p, "board-teach-go", "Teach", theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| {
                this.teach_board(window, cx);
                this.ask_rules();
            }))));
        let mut col = v_flex().w_full().gap(theme::z(10.)).child(teach);
        let Some((_, got)) = self.board_rules.clone() else { return col.child(quiet("Pick a project to see its rules.".into())).into_any_element() };
        match got {
            None => col = col.child(quiet("Reading them…".into())),
            Some(Err(e)) => col = col.child(quiet(e)),
            Some(Ok(book)) => {
                let group = |title: &str, rows: Vec<(String, String, String)>, col: Div| -> Div {
                    if rows.is_empty() {
                        return col;
                    }
                    let mut col = col.child(
                        div().pt(theme::z(20.)).pb(theme::z(4.)).font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(20.)).text_color(p.label).child(title.to_owned()),
                    );
                    for (name, how, path) in rows {
                        col = col.child(
                            h_flex()
                                .w_full()
                                .px(theme::z(GUTTER))
                                .py(theme::z(14.))
                                .gap(theme::z(16.))
                                .items_center()
                                .rounded(theme::z(theme::R_CONTROL + 14. / theme::SQUIRCLE))
                                .bg(p.raised)
                                .border_1()
                                .border_color(p.separator)
                                .child(v_flex().flex_1().min_w_0().child(div().truncate().text_size(theme::z(15.)).font_weight(FontWeight::MEDIUM).text_color(p.label).child(name)).child(div().truncate().text_size(theme::z(13.)).font_family(theme::mono()).text_color(p.tertiary).child(path)))
                                .child(div().flex_shrink_0().text_size(theme::z(13.)).text_color(p.secondary).child(how)),
                        );
                    }
                    col
                };
                let how = |c: &tessel_proto::proof::Check| match c {
                    tessel_proto::proof::Check::Judge => "Reviewed",
                    tessel_proto::proof::Check::Run { .. } => "Runs a command",
                    tessel_proto::proof::Check::Match { .. } => "Pattern",
                    tessel_proto::proof::Check::Look => "On screenshots",
                    tessel_proto::proof::Check::Unknown => "",
                };
                let rules: Vec<_> = book.rules.iter().filter(|r| !r.builtin).map(|r| (r.title.clone(), how(&r.check).to_owned(), r.path.clone())).collect();
                let knowledge: Vec<_> = book.knowledge.iter().map(|k| (k.title.clone(), String::new(), k.path.clone())).collect();
                let scenarios: Vec<_> = book.scenarios.iter().map(|s| (s.title.clone(), if s.holdout { "Hidden".into() } else if s.run.is_some() { "Script".into() } else { "Tester".into() }, s.path.clone())).collect();
                let empty = rules.is_empty() && knowledge.is_empty() && scenarios.is_empty();
                col = group("Rules", rules, col);
                col = group("Knowledge", knowledge, col);
                col = group("Scenarios", scenarios, col);
                if empty {
                    col = col.child(quiet("No rules yet.".into()));
                }
            }
        }
        col.into_any_element()
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

    pub(crate) fn ask_issues(&mut self) {
        let Some(repo) = self.board_repo() else { return };
        self.board_issues = Some((repo.clone(), Ok(Vec::new())));
        self.issues_loading = true;
        self.cmd(Cmd::Ask { tag: "issues", req: Request::IssueList { repo } });
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

    pub(crate) fn factory_paused(&self) -> bool {
        self.daemon_settings.as_ref().is_some_and(|d| d.paused)
    }

    pub(crate) fn set_paused(&mut self, paused: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(e) = self.exec("daemon.set", &serde_json::json!({"paused": paused}), window, cx) {
            self.error = Some(e);
        }
        cx.notify();
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
    theme::button(p, id, label.to_owned(), None, kind).h(theme::z(36.)).px(theme::z(16.)).text_size(theme::z(14.)).rounded(theme::z(theme::R_CONTROL)).on_click(on_click).test_support()
}
