use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use tessel_proto::task::{Task, TaskAct, TaskState};
use tessel_proto::{Request, Response};

const GUTTER: f32 = 20.;
const ROUND: f32 = theme::R_CONTROL + GUTTER;

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

    fn task_cost(&self, t: &Task) -> f64 {
        t.proof_cost_usd + t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).map_or(0.0, |s| s.cost_usd)
    }


    pub(crate) fn board_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let body = v_flex()
            .w_full()
            .max_w(theme::z(880.))
            .mx_auto()
            .px(theme::z(GUTTER))
            .pb(theme::z(48.))
            .child(self.factory_bar(cx))
            .child(self.scope_row(cx))
            .child(match self.board_tab {
                "issues" => self.issues_tab(cx),
                "rules" => self.rules_tab(cx),
                _ => self.tasks_tab(cx),
            });
        v_flex().id("board").size_full().overflow_y_scroll().child(body).into_any_element()
    }

    fn factory_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let paused = self.factory_paused();
        let working = self.tasks.iter().filter(|t| matches!(t.state, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking)).count();
        let now = super::now_ms();
        let today = super::views::spend_by_day(&self.metrics.as_ref().map(|m| m.spend.clone()).unwrap_or_default(), now, 1).last().map_or(0.0, |d| d.1);
        let plan = self.usage.iter().filter(|w| w.resets_at.is_none_or(|r| r * 1000 > now as i64)).map(|w| w.used).fold(None, |m: Option<f64>, u| Some(m.map_or(u, |m| m.max(u))));
        let mut facts = vec![format!("{working} working")];
        match plan {
            Some(used) => facts.push(format!("{:.0}% of plan", used * 100.0)),
            None => facts.push(format!("${today:.2} today")),
        }
        h_flex()
            .id("factory-bar")
            .test_support()
            .h(theme::z(52.))
            .gap(theme::z(12.))
            .items_center()
            .border_b_1()
            .border_color(p.separator)
            .child(div().text_size(theme::z(14.)).font_weight(FontWeight::SEMIBOLD).text_color(if paused { p.blocked } else { p.finished }).child(if paused { "Paused" } else { "Running" }))
            .child(div().flex_1().min_w_0().truncate().text_size(theme::z(14.)).text_color(p.secondary).font_features(theme::tabular()).child(facts.join("   ")))
            .child(big_button(&p, "board-pause", if paused { "Resume" } else { "Pause" }, if paused { theme::ButtonKind::Primary } else { theme::ButtonKind::Secondary }, cx.listener(move |this, _, window, cx| this.set_paused(!paused, window, cx))))
            .child(
                h_flex()
                    .id("factory-settings")
                    .test_support()
                    .size(theme::z(34.))
                    .justify_center()
                    .rounded(theme::z(theme::R_CONTROL))
                    .cursor_pointer()
                    .text_color(p.secondary)
                    .hover(|d| d.bg(p.hover).text_color(p.label))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let _ = this.exec("app.settings", &serde_json::json!({"section": "factory"}), window, cx);
                    }))
                    .child(Icon::empty().path("icons/settings.svg").size(theme::z(17.))),
            )
            .into_any_element()
    }

    fn scope_row(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let current = self.board_tab;
        let tab = |id: &'static str, label: &'static str| {
            let on = current == id;
            theme::segment(&p, SharedString::from(format!("board-tab-{id}")), label, on).test_support().h_full().px(theme::z(16.)).text_size(theme::z(14.)).on_click(cx.listener(move |this, _, _, cx| {
                this.board_tab = id;
                match id {
                    "issues" => this.ask_issues(),
                    "rules" => this.ask_rules(),
                    _ => {}
                }
                cx.notify();
            }))
        };
        let tabs = theme::tray(&p).h(theme::z(38.)).child(tab("tasks", "Tasks")).child(tab("issues", "Issues")).child(tab("rules", "Rules"));
        h_flex()
            .pt(theme::z(28.))
            .pb(theme::z(20.))
            .gap(theme::z(16.))
            .items_center()
            .child(self.scope_picker(cx))
            .child(div().flex_1())
            .child(tabs)
            .into_any_element()
    }

    fn scope_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let name = self.board_project.as_deref().map(super::projects::folder_name).unwrap_or_else(|| "All projects".into());
        let me = cx.entity().downgrade();
        Popover::new("board-project-popover")
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(
                super::composer::tool_button("board-project")
                    .h(theme::z(44.))
                    .px(theme::z(6.))
                    .ml(theme::z(-6.))
                    .child(h_flex().gap(theme::z(8.)).items_center().child(div().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(30.)).child(name)).child(Icon::empty().path("icons/chevron-down.svg").size(theme::z(16.)))),
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
                        h_flex()
                            .id(SharedString::from(format!("board-project-{i}")))
                            .test_support()
                            .min_h(theme::z(40.))
                            .px(theme::z(12.))
                            .gap(theme::z(10.))
                            .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                            .cursor_pointer()
                            .when(on, |d| d.bg(p.selection))
                            .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    this.board_project = pick.clone();
                                    this.board_issues = None;
                                    this.board_rules = None;
                                    cx.notify();
                                });
                            })
                            .child(div().flex_1().min_w_0().truncate().set(Type::Body).text_color(p.label).child(name))
                            .when(on, |d| d.child(Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.accent))),
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
        let picker = Popover::new("board-land-popover")
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
                        h_flex()
                            .id(("board-land-option", i))
                            .test_support()
                            .min_h(theme::z(38.))
                            .px(theme::z(12.))
                            .rounded(theme::z(theme::R_FLOAT - theme::FLOAT_PAD))
                            .cursor_pointer()
                            .when(on, |d| d.bg(p.selection))
                            .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                            .on_click(move |_, window, cx| {
                                popover.update(cx, |st, cx| st.dismiss(window, cx));
                                let _ = me.update(cx, |this, cx| {
                                    this.board_land = Some(value.to_owned());
                                    cx.notify();
                                });
                            })
                            .child(div().flex_1().set(Type::Body).text_color(p.label).child(label))
                            .when(on, |d| d.child(Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.accent))),
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

    fn tasks_tab(&self, cx: &mut Context<Self>) -> AnyElement {
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

        let mine = |t: &&Task| self.board_project.as_ref().is_none_or(|p| &t.repo == p);
        let by = |states: &[TaskState]| -> Vec<&Task> {
            let mut v: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state)).filter(mine).collect();
            v.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
            v
        };
        let needs = by(&[TaskState::Review]);
        let working = by(&[TaskState::Running, TaskState::Proving, TaskState::Landing, TaskState::Checking, TaskState::Planned]);
        let queued = by(&[TaskState::Ready]);
        let drafts = by(&[TaskState::Draft]);
        let done: Vec<&Task> = by(&[TaskState::Landed]).into_iter().filter(|t| !t.cleared).take(12).collect();

        let mut col = v_flex().w_full().gap(theme::z(12.)).child(composer);
        if !working.is_empty() || !queued.is_empty() {
            col = col.child(self.working_line(&working, &queued, cx));
        }
        let heading = |title: &str, n: usize| {
            h_flex()
                .pt(theme::z(28.))
                .pb(theme::z(4.))
                .gap(theme::z(10.))
                .items_baseline()
                .child(div().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(20.)).text_color(p.label).child(title.to_owned()))
                .child(div().text_size(theme::z(15.)).font_features(theme::tabular()).text_color(p.tertiary).child(n.to_string()))
        };
        if !needs.is_empty() {
            col = col.child(heading("Needs you", needs.len()));
            for t in needs {
                col = col.child(self.task_card(t, cx));
            }
        }
        if !done.is_empty() {
            let ids: Vec<String> = done.iter().map(|t| t.id.clone()).collect();
            col = col.child(heading("Done", done.len()).child(div().flex_1()).child(big_button(&p, "board-clear-done", "Clear", theme::ButtonKind::Ghost, cx.listener(move |this, _, _, cx| {
                for id in &ids {
                    this.cmd(Cmd::Ask { tag: "task", req: Request::TaskAct { id: id.clone(), act: TaskAct::Clear } });
                }
                cx.notify();
            }))));
            for t in done {
                col = col.child(self.task_card(t, cx));
            }
        }
        if !drafts.is_empty() {
            col = col.child(heading("Drafts", drafts.len()));
            for t in drafts {
                col = col.child(self.task_card(t, cx));
            }
        }
        col.into_any_element()
    }

    fn working_line(&self, working: &[&Task], queued: &[&Task], cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let open = self.show_working;
        let mut words = Vec::new();
        if !working.is_empty() {
            words.push(format!("{} working", working.len()));
        }
        if !queued.is_empty() {
            words.push(format!("{} queued", queued.len()));
        }
        let texture = self.field_on() && !working.is_empty();
        let mut col = v_flex()
            .w_full()
            .rounded(theme::z(26.))
            .bg(p.fill)
            .child(
                h_flex()
                    .id("board-working")
                    .relative()
                    .when(texture, |d| d.child(div().absolute().inset_0().child(self.field_el(cx))))
                    .test_support()
                    .h(theme::z(52.))
                    .px(theme::z(20.))
                    .gap(theme::z(10.))
                    .items_center()
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_working = !this.show_working;
                        cx.notify();
                    }))
                    .when(!working.is_empty(), |d| d.child(super::live_dot(p.accent, &p)))
                    .child(div().flex_1().text_size(theme::z(15.)).font_weight(FontWeight::MEDIUM).text_color(p.label).child(words.join(", ")))
                    .child(Icon::empty().path(if open { "icons/chevron-up.svg" } else { "icons/chevron-down.svg" }).size(theme::z(15.)).text_color(p.secondary)),
            );
        if open {
            for t in working.iter().chain(queued.iter()) {
                let id = t.id.clone();
                let session = t.session;
                col = col.child(
                    h_flex()
                        .id(SharedString::from(format!("working-{id}")))
                        .test_support()
                        .min_h(theme::z(44.))
                        .px(theme::z(20.))
                        .gap(theme::z(12.))
                        .items_center()
                        .border_t_1()
                        .border_color(p.separator)
                        .when(session.is_some(), |d| d.cursor_pointer().hover(|d| d.bg(p.hover)))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(s) = session {
                                this.switch_open(s, false, cx);
                                cx.notify();
                            }
                        }))
                        .child(div().flex_1().min_w_0().truncate().text_size(theme::z(14.)).text_color(p.label).child(t.title.clone()))
                        .child(div().flex_shrink_0().text_size(theme::z(13.)).text_color(p.secondary).child(self.doing(t)))
                        .child(
                            h_flex()
                                .id(SharedString::from(format!("task-drop-{id}")))
                                .test_support()
                                .size(theme::z(28.))
                                .justify_center()
                                .rounded(theme::z(8.))
                                .cursor_pointer()
                                .text_color(p.tertiary)
                                .hover(|d| d.bg(p.hover).text_color(p.label))
                                .on_click(cx.listener(move |this, _, _, cx| this.act_task(&id, TaskAct::Drop, cx)))
                                .child(Icon::empty().path("icons/x.svg").size(theme::z(13.))),
                        ),
                );
            }
        }
        col.into_any_element()
    }

    fn doing(&self, t: &Task) -> String {
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

    fn outcome(&self, t: &Task) -> String {
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

    fn task_card(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let id = t.id.clone();
        let open = self.open_task.as_deref() == Some(id.as_str());
        let tone = match t.state {
            TaskState::Review => p.blocked,
            TaskState::Landed => p.finished,
            _ => p.secondary,
        };
        let line = self.outcome(t);
        let toggle = id.clone();
        let mut card = v_flex()
            .id(ElementId::from(SharedString::from(format!("task-{id}"))))
            .test_support()
            .aria_label(SharedString::from(t.title.clone()))
            .w_full()
            .rounded(theme::z(ROUND))
            .bg(p.raised)
            .border_1()
            .border_color(p.separator)
            .when(!crate::glass::liquid(), |d| d.shadow_sm())
            .child(
                v_flex()
                    .id(SharedString::from(format!("task-head-{id}")))
                    .px(theme::z(GUTTER))
                    .py(theme::z(GUTTER))
                    .gap(theme::z(4.))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_task = if this.open_task.as_deref() == Some(toggle.as_str()) { None } else { Some(toggle.clone()) };
                        cx.notify();
                    }))
                    .child(div().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::SEMIBOLD).text_size(theme::z(17.)).line_height(theme::z(23.)).text_color(p.label).child(t.title.clone()))
                    .when(!line.is_empty(), |d| d.child(div().text_size(theme::z(14.)).line_height(theme::z(20.)).text_color(tone).child(line))),
            );
        if open {
            card = card.child(self.task_details(t, cx));
        } else if t.state == TaskState::Review {
            card = card.child(self.task_actions(t, cx).px(theme::z(GUTTER)).pb(theme::z(GUTTER)));
        } else if t.state == TaskState::Draft {
            card = card.child(self.task_actions(t, cx).px(theme::z(GUTTER)).pb(theme::z(GUTTER)));
        }
        card.into_any_element()
    }

    fn task_actions(&self, t: &Task, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let id = t.id.clone();
        let mut row = h_flex().w_full().flex_wrap().gap(theme::z(10.)).items_center();
        match t.state {
            TaskState::Review => {
                let (i, j) = (id.clone(), id.clone());
                row = row
                    .child(big_button(&p, format!("task-land-{id}"), "Land", theme::ButtonKind::Primary, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))))
                    .child(big_button(&p, format!("task-change-{id}"), "Ask for Changes", theme::ButtonKind::Secondary, cx.listener(move |this, _, window, cx| {
                        this.changing_task = Some(j.clone());
                        this.task_change.update(cx, |s, cx| s.focus(window, cx));
                        cx.notify();
                    })));
            }
            TaskState::Draft => {
                let (i, j) = (id.clone(), id.clone());
                row = row
                    .child(big_button(&p, format!("task-ready-{id}"), "Start", theme::ButtonKind::Primary, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Ready, cx))))
                    .child(big_button(&p, format!("task-plan-{id}"), "Plan", theme::ButtonKind::Secondary, cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Plan, cx))));
            }
            _ => {}
        }
        if let Some(url) = t.pr.clone() {
            row = row.child(big_button(&p, format!("task-pr-{id}"), "Pull Request", theme::ButtonKind::Secondary, cx.listener(move |_, _, _, cx| cx.open_url(&url))));
        }
        if let Some(url) = t.issue.clone() {
            let label = url.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "Issue".to_owned(), |n| format!("Issue #{n}"));
            row = row.child(big_button(&p, format!("task-issue-{id}"), &label, theme::ButtonKind::Secondary, cx.listener(move |_, _, _, cx| cx.open_url(&url))));
        }
        if let Some(s) = t.session {
            row = row.child(big_button(&p, format!("task-chat-{id}"), "Open Chat", theme::ButtonKind::Ghost, cx.listener(move |this, _, _, cx| {
                this.switch_open(s, false, cx);
                cx.notify();
            })));
        }
        if !t.state.finished() {
            let i = id.clone();
            row = row.child(div().flex_1()).child(big_button(&p, format!("task-drop-{id}"), "Drop", theme::ButtonKind::Ghost, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Drop, cx))));
        }
        let changing = self.changing_task.as_deref() == Some(id.as_str());
        v_flex()
            .w_full()
            .gap(theme::z(12.))
            .child(row)
            .when(changing, |d| {
                d.child(
                    h_flex()
                        .w_full()
                        .gap(theme::z(10.))
                        .items_center()
                        .child(div().flex_1().h(theme::z(40.)).px(theme::z(14.)).flex().items_center().rounded(theme::z(theme::R_CONTROL)).border_1().border_color(p.separator).bg(p.window).text_size(theme::z(15.)).child(div().w_full().child(Input::new(&self.task_change).id("task-change").appearance(false))))
                        .child(big_button(&p, "task-change-rule", "Send and Remember", theme::ButtonKind::Secondary, cx.listener(|this, _, window, cx| this.change_task_and_teach(window, cx))))
                        .child(big_button(&p, "task-change-send", "Send", theme::ButtonKind::Primary, cx.listener(|this, _, window, cx| this.change_task(window, cx)))),
                )
            })
    }

    fn task_details(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let proof = t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).and_then(|s| s.proof.clone());
        let label = |text: &str| div().w(theme::z(120.)).flex_shrink_0().pt(theme::z(1.)).text_size(theme::z(13.)).font_weight(FontWeight::MEDIUM).text_color(p.secondary).child(text.to_owned());
        let row = |name: &str, body: AnyElement| h_flex().w_full().gap(theme::z(16.)).items_start().child(label(name)).child(div().flex_1().min_w_0().child(body));
        let text = |s: String| div().text_size(theme::z(14.)).line_height(theme::z(21.)).text_color(p.label).child(s).into_any_element();
        let mut col = v_flex().w_full().px(theme::z(GUTTER)).pb(theme::z(20.)).gap(theme::z(16.)).border_t_1().border_color(p.separator).pt(theme::z(18.));
        if let Some(r) = &t.review {
            col = col.child(row("Reviewer", text(r.clone())));
        }
        if let Some(s) = &t.summary {
            col = col.child(row("What it did", text(s.chars().take(700).collect())));
        }
        if !t.changes.is_empty() {
            let mut files = v_flex().gap(theme::z(4.));
            for f in t.changes.iter().take(14) {
                files = files.child(
                    h_flex()
                        .gap(theme::z(10.))
                        .text_size(theme::z(13.))
                        .child(div().flex_1().min_w_0().truncate().font_family(theme::mono()).text_color(p.label).child(f.path.clone()))
                        .child(div().w(theme::z(44.)).text_right().font_features(theme::tabular()).text_color(p.finished).child(if f.added > 0 { format!("+{}", f.added) } else { String::new() }))
                        .child(div().w(theme::z(44.)).text_right().font_features(theme::tabular()).text_color(p.failed).child(if f.removed > 0 { format!("−{}", f.removed) } else { String::new() })),
                );
            }
            if t.changes.len() > 14 {
                files = files.child(div().text_size(theme::z(13.)).text_color(p.secondary).child(format!("and {} more files", t.changes.len() - 14)));
            }
            col = col.child(row("Changed", files.into_any_element()));
        }
        if let Some(proof) = &proof {
            let mut checks = v_flex().gap(theme::z(6.));
            for v in proof.verify.iter().filter(|v| !v.ok) {
                checks = checks.child(text(format!("`{}` fails", v.command)));
            }
            for tr in proof.tried.iter().filter(|t| !t.ok) {
                checks = checks.child(text(format!("{} doesn't work", tr.title)));
            }
            for f in proof.standing().filter(|f| f.severity != tessel_proto::proof::Severity::Note).take(8) {
                let at = match (&f.file, f.line) {
                    (Some(file), Some(l)) => format!(" ({file}:{l})"),
                    (Some(file), None) => format!(" ({file})"),
                    _ => String::new(),
                };
                checks = checks.child(text(format!("{}{at}", f.title)));
            }
            let all_good = !proof.verify.iter().any(|v| !v.ok) && !proof.tried.iter().any(|t| !t.ok) && !proof.standing().any(|f| f.severity != tessel_proto::proof::Severity::Note);
            if all_good {
                let n = proof.verify.len() + proof.tried.len();
                checks = checks.child(text(if n > 0 { format!("All {n} pass, and the reviewers found nothing") } else { "The reviewers found nothing".into() }));
            }
            col = col.child(row("Checks", checks.into_any_element()));
        }
        let cost = self.task_cost(t);
        if cost >= 0.005 {
            col = col.child(row("Cost", text(format!("${cost:.2}"))));
        }
        col.child(self.task_actions(t, cx)).into_any_element()
    }

    fn issues_tab(&self, cx: &mut Context<Self>) -> AnyElement {
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
                            .rounded(theme::z(theme::R_CONTROL + 14.))
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

    fn rules_tab(&self, cx: &mut Context<Self>) -> AnyElement {
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
                                .rounded(theme::z(theme::R_CONTROL + 14.))
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
