use super::*;
use gpui_kit::component::Icon;
use tessel_proto::Request;
use tessel_proto::task::{Task, TaskAct, TaskState};

type Click = Box<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

const RAIL: f32 = 300.;
const MAT: f32 = 34.;
const MAT_PAD: f32 = 14.;

const BOTHER: [(&str, &str); 5] = [("me", "Bother me for anything"), ("agent", "Only critical decisions"), ("ui", "Only UI choices"), ("full", "Never bother me"), ("custom", "My own rule…")];

pub(crate) enum Focus<'a> {
    Task(&'a Task),
    New,
    Issues,
    Rules,
}

impl Shell {
    fn stage_mine(&self, t: &Task) -> bool {
        self.board_project.as_ref().is_none_or(|p| &t.repo == p)
    }

    fn stage_list(&self, states: &[TaskState]) -> Vec<&Task> {
        let mut v: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state) && self.stage_mine(t) && !t.cleared).collect();
        v.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        v
    }

    pub(crate) fn stage_focus(&self) -> Focus<'_> {
        match self.stage_pick.as_deref() {
            Some("new") => return Focus::New,
            Some("issues") => return Focus::Issues,
            Some("rules") => return Focus::Rules,
            Some(id) => {
                if let Some(t) = self.tasks.iter().find(|t| t.id == id) {
                    return Focus::Task(t);
                }
            }
            None => {}
        }
        let first = |states: &[TaskState]| self.stage_list(states).into_iter().next();
        first(&[TaskState::Review])
            .or_else(|| first(&[TaskState::Running, TaskState::Proving, TaskState::Landing, TaskState::Checking, TaskState::Planned]))
            .map_or(Focus::New, Focus::Task)
    }

    fn stage_pick_now(&mut self, pick: Option<String>) {
        self.stage_pick = pick;
        self.stage_shot = 0;
        self.stage_swatch = None;
        self.changing_task = None;
    }

    pub(crate) fn want_stage_diff(&mut self) {
        let Focus::Task(t) = self.stage_focus() else { return };
        if self.stage_diff_for.as_deref() == Some(t.id.as_str()) {
            return;
        }
        let at = t.landed.clone().filter(|s| s.len() >= 7 && s.chars().all(|c| c.is_ascii_hexdigit()));
        let (id, session, paths) = (t.id.clone(), t.session, t.changes.iter().map(|f| f.path.clone()).collect::<Vec<_>>());
        self.stage_diff_for = Some(id);
        self.stage_diff_parts.clear();
        self.stage_diff = None;
        let Some(session) = session else { return };
        for path in paths {
            self.cmd(Cmd::Ask { tag: "stage.diff", req: Request::GitDiff { session, path, at: at.clone(), index: None } });
        }
    }

    pub(crate) fn stage_diff_answer(&mut self, result: Result<tessel_proto::Response, String>, cx: &mut Context<Self>) {
        use gpui_kit::component::diff::{DiffFile, DiffMode, DiffState};
        let Ok(tessel_proto::Response::Diff { diff }) = result else { return };
        if diff.binary || diff.unified.trim().is_empty() {
            return;
        }
        let order: Vec<String> = self.stage_diff_for.as_ref().and_then(|id| self.tasks.iter().find(|t| &t.id == id)).map(|t| t.changes.iter().map(|f| f.path.clone()).collect()).unwrap_or_default();
        self.stage_diff_parts.retain(|d| d.path != diff.path);
        self.stage_diff_parts.push(diff);
        self.stage_diff_parts.sort_by_key(|d| order.iter().position(|p| p == &d.path).unwrap_or(usize::MAX));
        let unified = self.stage_diff_parts.iter().map(|d| d.unified.trim_end()).collect::<Vec<_>>().join("\n");
        let files = DiffFile::parse(&format!("{unified}\n")).unwrap_or_default();
        match &self.stage_diff {
            Some(state) => state.update(cx, |s, cx| s.set_files(files, cx)),
            None => self.stage_diff = Some(cx.new(|cx| DiffState::new(files, cx).with_mode(DiffMode::Unified))),
        }
        cx.notify();
    }

    fn stage_diff_el(&self, p: &Palette) -> Option<AnyElement> {
        let state = self.stage_diff.as_ref()?;
        let lines: usize = self.stage_diff_parts.iter().map(|d| d.unified.lines().count()).sum();
        let h = (lines as f32 * 20. + self.stage_diff_parts.len() as f32 * 44. + 16.).clamp(180., 720.);
        Some(
            div()
                .id("stage-diff")
                .test_support()
                .w_full()
                .h(theme::z(h))
                .rounded(theme::z(24.))
                .overflow_hidden()
                .border_1()
                .border_color(p.separator)
                .bg(p.raised)
                .child(gpui_kit::component::diff::Diff::new(state).size_full().border_0())
                .into_any_element(),
        )
    }

    pub(crate) fn stage_main(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        if matches!(self.stage_focus(), Focus::New) && self.slack_channels.is_none() {
            let me = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = me.update(cx, |this, cx| {
                    this.want_slack();
                    cx.notify();
                });
            });
        }
        if matches!(self.stage_focus(), Focus::New)
            && let Some(repo) = self.board_repo()
            && self.board_issues.as_ref().is_none_or(|(r, _)| *r != repo)
        {
            let me = cx.entity().downgrade();
            cx.defer(move |cx| {
                let _ = me.update(cx, |this, cx| {
                    this.ask_issues();
                    cx.notify();
                });
            });
        }
        if let Focus::Task(t) = self.stage_focus()
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
        let main = match self.stage_focus() {
            Focus::Task(t) => self.stage_task(t, cx),
            Focus::New => v_flex().w_full().max_w(theme::z(820.)).mx_auto().px(theme::z(48.)).pt(theme::z(56.)).pb(theme::z(48.)).gap(theme::z(36.)).child(self.task_composer(cx)).children(self.task_starts(cx)).into_any_element(),
            Focus::Issues => v_flex().w_full().max_w(theme::z(880.)).mx_auto().px(theme::z(48.)).pt(theme::z(36.)).pb(theme::z(48.)).child(self.issues_tab(cx)).into_any_element(),
            Focus::Rules => v_flex().w_full().max_w(theme::z(880.)).mx_auto().px(theme::z(48.)).pt(theme::z(36.)).pb(theme::z(48.)).child(self.rules_tab(cx)).into_any_element(),
        };
        div().id("stage").size_full().overflow_y_scroll().bg(p.window).child(main).into_any_element()
    }

    pub(crate) fn stage_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        h_flex()
            .id("board")
            .size_full()
            .items_start()
            .child(div().w(theme::z(RAIL)).flex_shrink_0().h_full().border_r_1().border_color(p.separator).child(self.stage_rail_body(cx)))
            .child(div().flex_1().min_w_0().h_full().child(self.stage_main(cx)))
            .into_any_element()
    }

    pub(crate) fn view_switch(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let factory = self.factory_view;
        let needs = self.tasks.iter().filter(|t| t.state == TaskState::Review && !t.cleared).count();
        let seg = |id: &'static str, label: &'static str, on: bool, view: &'static str, badge: usize| {
            theme::segment(&p, id, label, on)
                .rounded(theme::z(theme::nested(theme::by_height(theme::SIDE_ROW_H), theme::TRAY_PAD)))
                .flex_1()
                .justify_center()
                .gap(theme::z(6.))
                .h_full()
                .text_size(theme::z(13.))
                .when(badge > 0, |d| d.child(div().text_size(theme::z(12.)).font_features(theme::tabular()).text_color(p.blocked).child(badge.to_string())))
                .on_click(cx.listener(move |this, _, window, cx| {
                    let _ = this.exec("app.view", &serde_json::json!({"view": view}), window, cx);
                }))
                .test_support()
        };
        div()
            .px(theme::z(theme::SIDE_INSET))
            .pb(theme::z(10.))
            .child(theme::tray(&p).w_full().h(theme::z(theme::SIDE_ROW_H)).rounded(theme::z(theme::by_height(theme::SIDE_ROW_H))).child(seg("view-chats", "Chats", !factory, "chats", 0)).child(seg("view-factory", "Factory", factory, "factory", if factory { 0 } else { needs })))
            .into_any_element()
    }

    pub(crate) fn stage_rail_body(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let paused = self.factory_paused();
        let focus_id = match self.stage_focus() {
            Focus::Task(t) => t.id.clone(),
            Focus::New => "new".into(),
            Focus::Issues => "issues".into(),
            Focus::Rules => "rules".into(),
        };
        let pick = |pick: &'static str, cx: &mut Context<Self>| -> Click {
            Box::new(cx.listener(move |this: &mut Shell, _: &ClickEvent, _: &mut Window, cx: &mut Context<Shell>| {
                this.stage_pick_now(Some(pick.to_owned()));
                match pick {
                    "issues" => this.ask_issues(),
                    "rules" => this.ask_rules(),
                    _ => {}
                }
                cx.notify();
            }))
        };
        let nav_row = |id: &'static str, icon: &str, label: String, on: bool| {
            theme::side_row(&p, id, on, theme::SIDE_ROW_H)
                .items_center()
                .child(Icon::empty().path(format!("icons/{icon}.svg")).size(theme::z(15.)).text_color(p.secondary))
                .child(div().flex_1().min_w_0().truncate().set(Type::Body).text_color(p.label).child(label))
        };
        let needs = self.stage_list(&[TaskState::Review]);
        let busy = self.stage_list(&[TaskState::Running, TaskState::Proving, TaskState::Landing, TaskState::Checking, TaskState::Planned]);
        let next = self.stage_list(&[TaskState::Ready]);
        let landed: Vec<&Task> = self.stage_list(&[TaskState::Landed]).into_iter().take(12).collect();
        let drafts = self.stage_list(&[TaskState::Draft]);
        let issues = self.board_issues.as_ref().and_then(|(_, got)| got.as_ref().ok()).map_or(0, |v| v.len());
        let nav = v_flex()
            .flex_shrink_0()
            .gap(theme::z(1.))
            .child(div().mx(theme::z(theme::SIDE_INSET)).mb(theme::z(6.)).child(self.host_picker(cx)))
            .child(nav_row("stage-new", "square-pen", "New task".into(), focus_id == "new").on_click(pick("new", cx)).test_support())
            .child(nav_row("board-tab-issues", "circle-dot", "Issues".into(), focus_id == "issues").when(issues > 0, |d| d.child(theme::count_pill(&p, issues.to_string(), false))).on_click(pick("issues", cx)).test_support())
            .child(nav_row("board-tab-rules", "book-open", "Rules".into(), focus_id == "rules").on_click(pick("rules", cx)).test_support())
            .child(
                nav_row("board-pause", if paused { "play" } else { "pause" }, if paused { "Resume".into() } else { "Pause".into() }, false)
                    .when(paused, |d| d.child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.blocked).child("Paused")))
                    .on_click(cx.listener(move |this, _, window, cx| this.set_paused(!paused, window, cx)))
                    .test_support(),
            );
        let mut projects = vec![(String::new(), "All projects".to_owned())];
        projects.extend(self.board_projects().into_iter().map(|f| (f.clone(), super::projects::folder_name(&f))));
        let project = self.board_project.clone().unwrap_or_default();
        let project_label = projects.iter().find(|(v, _)| *v == project).map_or_else(|| "All projects".to_owned(), |(_, l)| l.clone());
        let scope = self.select(
            super::select::Select { id: "board-project", label: project_label, options: projects, current: project, width: None, height: theme::SIDE_ROW_H, up: false },
            std::rc::Rc::new(|this: &mut Shell, value: &str, _: &mut Window, _: &mut Context<Shell>| {
                this.board_project = (!value.is_empty()).then(|| value.to_owned());
                this.board_issues = None;
                this.board_rules = None;
            }),
            cx,
        );
        let controls = v_flex().px(theme::z(theme::SIDE_INSET)).pt(theme::z(10.)).gap(theme::z(6.)).child(scope).child(self.bother_picker(cx));

        let row = |id: String, pick: String, body: Div, cx: &mut Context<Self>| {
            let on = focus_id == pick;
            theme::side_row(&p, SharedString::from(id), on, theme::SIDE_TWO_H)
                .items_center()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.stage_pick_now(Some(pick.clone()));
                    cx.notify();
                }))
                .test_support()
                .child(body)
        };
        let heading = |text: &str| div().px(theme::z(theme::SIDE_INSET + theme::SIDE_PAD)).pt(theme::z(16.)).pb(theme::z(4.)).set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(text.to_owned());
        let title = |t: &Task, color: Hsla| div().set(Type::Body).font_weight(FontWeight::MEDIUM).text_color(color).truncate().child(t.title.clone());
        let sub = |text: String, color: Hsla| div().set(Type::Subheadline).text_color(color).truncate().child(text);
        let dotted = |t: &Task, core: Hsla, live: bool, line: String, line_color: Hsla| {
            v_flex()
                .flex_1()
                .min_w_0()
                .gap(theme::z(2.))
                .child(title(t, p.label))
                .child(h_flex().gap(theme::z(6.)).items_center().child(div().w(theme::z(12.)).flex_shrink_0().flex().justify_center().child(super::status_dot(core, live, &p))).child(div().flex_1().min_w_0().child(sub(line, line_color))))
        };
        let mut list = v_flex().pb(theme::z(12.));
        if !needs.is_empty() {
            list = list.child(heading("Needs you"));
            for t in needs {
                let line = if t.options.is_empty() { "Waiting on you".to_owned() } else { format!("{} options", t.options.len()) };
                list = list.child(row(format!("stage-task-{}", t.id), t.id.clone(), dotted(t, p.blocked, false, line, p.secondary), cx));
            }
        }
        if !busy.is_empty() {
            list = list.child(heading("Working"));
            for t in busy {
                let line = self.stage_doing(t);
                list = list.child(row(format!("stage-task-{}", t.id), t.id.clone(), dotted(t, p.info, true, line, p.secondary), cx));
            }
        }
        if !next.is_empty() {
            list = list.child(heading("Next"));
            for t in next {
                let line = if t.deps.is_empty() { "Queued".to_owned() } else { "After another task".into() };
                list = list.child(row(format!("stage-task-{}", t.id), t.id.clone(), dotted(t, p.tertiary, false, line, p.tertiary), cx));
            }
        }
        if !landed.is_empty() {
            let ids: Vec<String> = landed.iter().map(|t| t.id.clone()).collect();
            list = list.child(
                h_flex()
                    .pr(theme::z(theme::SIDE_INSET))
                    .items_end()
                    .child(heading("Landed").flex_1())
                    .child(
                        theme::button(&p, "board-clear-done", "Clear", None, theme::ButtonKind::Ghost)
                            .h(theme::z(24.))
                            .px(theme::z(8.))
                            .rounded(theme::z(theme::by_height(24.)))
                            .text_color(p.secondary)
                            .on_click(cx.listener(move |this, _, _, cx| {
                                for id in &ids {
                                    this.cmd(Cmd::Ask { tag: "task", req: Request::TaskAct { id: id.clone(), act: TaskAct::Clear } });
                                }
                                cx.notify();
                            }))
                            .test_support(),
                    ),
            );
            for t in landed {
                let mark = t.landed.as_deref().map(|s| if s.starts_with("http") { "Pull request merged".to_owned() } else if s.contains('/') { format!("Kept on {s}") } else { s.chars().take(7).collect() }).unwrap_or_default();
                list = list.child(row(format!("stage-task-{}", t.id), t.id.clone(), dotted(t, p.finished, false, mark, p.tertiary), cx));
            }
        }
        if !drafts.is_empty() {
            list = list.child(heading("Drafts"));
            for t in drafts {
                let line = if t.spec.trim().is_empty() { "Not started".to_owned() } else { t.spec.lines().next().unwrap_or_default().to_owned() };
                list = list.child(row(format!("stage-task-{}", t.id), t.id.clone(), dotted(t, p.quaternary, false, line, p.tertiary), cx));
            }
        }
        v_flex()
            .id("stage-rail")
            .size_full()
            .child(nav)
            .child(controls)
            .child(div().id("stage-rail-list").flex_1().min_h_0().overflow_y_scroll().child(list))
            .children(self.usage_block())
            .test_support()
            .into_any_element()
    }

    fn usage_block(&self) -> Option<AnyElement> {
        let p = self.palette.get();
        let now = (super::now_ms() / 1000) as i64;
        let mut windows: Vec<&tessel_proto::UsageWindow> = self.usage.iter().collect();
        if windows.is_empty() && self.account.is_none() {
            return None;
        }
        windows.sort_by_key(|w| (w.name != "five_hour", w.name.clone()));
        let mut col = v_flex().id("usage-limits").test_support().flex_shrink_0().mx(theme::z(theme::SIDE_INSET)).px(theme::z(theme::SIDE_PAD)).pt(theme::z(12.)).pb(theme::z(10.)).gap(theme::z(10.)).border_t_1().border_color(p.separator);
        if let Some(a) = &self.account {
            col = col.child(
                v_flex()
                    .id("usage-account")
                    .child(h_flex().gap(theme::z(6.)).items_baseline().child(div().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.label).child(if a.plan.is_empty() { "Claude".to_owned() } else { format!("Claude {}", a.plan) })))
                    .child(div().truncate().set(Type::Subheadline).text_color(p.secondary).child(a.email.clone())),
            );
        }
        for w in windows {
            let over = w.resets_at.is_some_and(|r| r <= now);
            let pct = if over { 0 } else { (w.used * 100.0).round().clamp(0.0, 100.0) as u32 };
            let tone = if (!over && w.status == "rejected") || pct >= 95 { p.failed } else if pct >= 70 { p.blocked } else { p.label };
            col = col.child(
                v_flex()
                    .gap(theme::z(5.))
                    .child(
                        h_flex()
                            .items_baseline()
                            .gap(theme::z(8.))
                            .child(div().flex_1().min_w_0().truncate().set(Type::Subheadline).font_weight(FontWeight::MEDIUM).text_color(p.label).child(super::composer::usage_name(&w.name)))
                            .child(div().flex_shrink_0().set(Type::Subheadline).font_features(theme::tabular()).text_color(if pct >= 70 { tone } else { p.secondary }).child(format!("{pct}%"))),
                    )
                    .child(div().h(theme::z(5.)).w_full().rounded_full().bg(p.fill).child(div().h_full().rounded_full().bg(tone).w(relative(pct as f32 / 100.0))))
                    .children(w.resets_at.filter(|_| !over).map(|t| div().set(Type::Subheadline).text_color(p.tertiary).child(format!("Resets {}", super::composer::resets(t))))),
            );
        }
        Some(col.into_any_element())
    }

    fn bother_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let (mode, rule) = self.daemon_settings.as_ref().map_or_else(|| ("agent".to_owned(), String::new()), |d| (d.autonomy.clone(), d.bother.clone()));
        let label = match mode.as_str() {
            "custom" if !rule.trim().is_empty() => rule.clone(),
            other => BOTHER.iter().find(|(v, _)| *v == other).map_or("Only critical decisions", |(_, l)| l).to_owned(),
        };
        let picker = self.select(
            super::select::Select {
                id: "bother",
                label,
                options: BOTHER.iter().map(|(v, l)| ((*v).to_owned(), (*l).to_owned())).collect(),
                current: mode,
                width: None,
                height: theme::SIDE_ROW_H,
                up: false,
            },
            std::rc::Rc::new(|this: &mut Shell, value: &str, window: &mut Window, cx: &mut Context<Shell>| {
                if value == "custom" {
                    this.editing_bother = true;
                    let rule = this.daemon_settings.as_ref().map(|d| d.bother.clone()).unwrap_or_default();
                    this.bother_input.update(cx, |s, cx| {
                        s.set_value(rule, window, cx);
                        s.focus(window, cx);
                    });
                } else {
                    this.editing_bother = false;
                    if let Err(e) = this.exec("daemon.set", &serde_json::json!({"autonomy": value}), window, cx) {
                        this.error = Some(e);
                    }
                }
            }),
            cx,
        );
        v_flex()
            .w_full()
            .gap(theme::z(8.))
            .child(picker)
            .when(self.editing_bother, |d| {
                d.child(
                    h_flex()
                        .w_full()
                        .h(theme::z(theme::SIDE_ROW_H))
                        .pl(theme::z(theme::SIDE_PAD))
                        .pr(theme::z(4.))
                        .gap(theme::z(6.))
                        .items_center()
                        .rounded(theme::z(theme::by_height(theme::SIDE_ROW_H)))
                        .border_1()
                        .border_color(p.separator)
                        .bg(p.window)
                        .child(div().flex_1().min_w_0().text_size(theme::z(14.)).child(Input::new(&self.bother_input).id("bother-rule").appearance(false)))
                        .child(
                            theme::button(&p, "bother-save", "Save", None, theme::ButtonKind::Primary)
                                .h(theme::z(26.))
                                .px(theme::z(10.))
                                .text_size(theme::z(13.))
                                .rounded(theme::z(theme::nested(theme::by_height(theme::SIDE_ROW_H), 4.)))
                                .on_click(cx.listener(|this, _, window, cx| this.save_bother(window, cx)))
                                .test_support(),
                        ),
                )
            })
            .into_any_element()
    }

    pub(crate) fn save_bother(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rule = self.bother_input.read(cx).value().trim().to_owned();
        if rule.is_empty() {
            return;
        }
        if let Err(e) = self.exec("daemon.set", &serde_json::json!({"autonomy": "custom", "bother": rule}), window, cx) {
            self.error = Some(e);
        }
        self.editing_bother = false;
        cx.notify();
    }

    fn stage_doing(&self, t: &Task) -> String {
        if let Some(i) = t.building {
            return format!("Trying option {} of {}", i + 1, t.options.len().max(i + 1));
        }
        self.doing(t)
    }

    fn shot_src(&self, path: &str) -> Option<ImageSource> {
        if std::path::Path::new(path).is_file() {
            Some(std::path::PathBuf::from(path).into())
        } else {
            self.shots.get(path).cloned().flatten().map(ImageSource::from)
        }
    }

    fn stage_task(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let swatched = t.options.iter().any(|o| !o.shots.is_empty());
        let shown = if swatched { self.stage_swatch.unwrap_or(0).min(t.options.len().saturating_sub(1)) } else { 0 };
        let pictures: Vec<String> = if swatched { t.options[shown].shots.clone() } else { t.shots.clone() };
        let at = self.stage_shot.min(pictures.len().saturating_sub(1));
        let mut col = v_flex().w_full().max_w(theme::z(1180.)).mx_auto().px(theme::z(48.)).pt(theme::z(28.)).pb(theme::z(48.)).gap(theme::z(20.));
        col = col.child(
            h_flex()
                .items_center()
                .gap(theme::z(6.))
                .child(div().flex_1().min_w_0().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::SEMIBOLD).text_size(theme::z(17.)).text_color(p.secondary).truncate().child(t.title.clone()))
                .when(self.task_cost(t) >= 0.005, |d| d.child(div().pr(theme::z(8.)).text_size(theme::z(14.)).font_features(theme::tabular()).text_color(p.tertiary).child(format!("${:.2}", self.task_cost(t)))))
                .child(self.stage_side_actions(t, cx)),
        );
        let headline = match t.state {
            TaskState::Review => t.ask.clone().unwrap_or_else(|| self.outcome(t)),
            TaskState::Landed => self.outcome(t),
            TaskState::Draft => if t.spec.trim().is_empty() { t.title.clone() } else { t.spec.lines().next().unwrap_or_default().to_owned() },
            _ => self.stage_doing(t),
        };
        col = col.child(div().max_w(theme::z(900.)).font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(32.)).line_height(theme::z(38.)).text_color(if t.state == TaskState::Landed { p.finished } else { p.label }).child(headline));
        if let Some(r) = t.review.clone().filter(|_| t.state == TaskState::Landed) {
            col = col.child(div().max_w(theme::z(720.)).text_size(theme::z(15.)).line_height(theme::z(22.)).text_color(p.secondary).child(r));
        }
        if let Some(row) = self.stage_answers(t, shown, swatched, cx) {
            col = col.child(row);
        }
        if self.changing_task.as_deref() == Some(t.id.as_str()) {
            col = col.child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap(theme::z(10.))
                    .items_center()
                    .child(div().flex_1().min_w(theme::z(240.)).h(theme::z(44.)).px(theme::z(16.)).flex().items_center().rounded(theme::z(22.)).border_1().border_color(p.separator).bg(p.raised).text_size(theme::z(15.)).child(div().w_full().child(Input::new(&self.task_change).id("task-change").appearance(false))))
                    .child(stage_button(&p, "task-change-rule", "Send and Remember", theme::ButtonKind::Secondary, false, cx.listener(|this, _, window, cx| this.change_task_and_teach(window, cx))))
                    .child(stage_button(&p, "task-change-send", "Send", theme::ButtonKind::Primary, false, cx.listener(|this, _, window, cx| this.change_task(window, cx)))),
            );
        }
        let diff = self.stage_diff_el(&p);
        if pictures.is_empty() {
            if let Some(row) = self.stage_checks_row(t, &p) {
                col = col.child(row);
            }
            if diff.is_none()
                && let Some(e) = self.stage_evidence(t, &p)
            {
                col = col.child(div().w_full().rounded(theme::z(24.)).bg(p.raised).border_1().border_color(p.separator).p(theme::z(20.)).child(e));
            }
        } else {
            let evidence = self.stage_evidence(t, &p);
            let shot = self.shot_src(&pictures[at]);
            let mat = div()
                .w_full()
                .h(theme::z(400.))
                .relative()
                .rounded(theme::z(MAT))
                .overflow_hidden()
                .bg(p.fill)
                .child(div().absolute().inset_0().child(self.mat_el(MAT, cx)))
                .child(
                    h_flex()
                        .relative()
                        .size_full()
                        .p(theme::z(MAT_PAD))
                        .gap(theme::z(MAT_PAD))
                        .child(
                            div()
                                .id("stage-shot")
                                .test_support()
                                .flex_1()
                                .min_w_0()
                                .h_full()
                                .rounded(theme::z(theme::nested(MAT, MAT_PAD)))
                                .overflow_hidden()
                                .bg(p.window)
                                .shadow_lg()
                                .when_some(shot, |d, src| d.child(img(src).size_full().object_fit(ObjectFit::Cover))),
                        )
                        .when_some(evidence, |d, e| d.child(div().w(theme::z(340.)).flex_shrink_0().h_full().rounded(theme::z(theme::nested(MAT, MAT_PAD))).bg(p.window.opacity(0.94)).p(theme::z(18.)).overflow_hidden().child(e))),
                );
            col = col.child(mat);
            if pictures.len() > 1 {
                let mut strip = h_flex().gap(theme::z(8.)).flex_wrap();
                for (i, path) in pictures.iter().enumerate() {
                    let on = i == at;
                    strip = strip.child(
                        div()
                            .id(("stage-thumb", i))
                            .test_support()
                            .w(theme::z(104.))
                            .h(theme::z(66.))
                            .rounded(theme::z(12.))
                            .overflow_hidden()
                            .bg(p.fill)
                            .border_2()
                            .border_color(if on { p.accent } else { p.separator })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stage_shot = i;
                                cx.notify();
                            }))
                            .when_some(self.shot_src(path), |d, src| d.child(img(src).size_full().object_fit(ObjectFit::Cover))),
                    );
                }
                col = col.child(strip);
            }
        }
        if let Some(d) = diff {
            col = col.child(d);
        }
        col.into_any_element()
    }

    fn stage_checks_row(&self, t: &Task, p: &Palette) -> Option<AnyElement> {
        let proof = t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).and_then(|s| s.proof.clone())?;
        let mut items: Vec<(Hsla, String)> = Vec::new();
        for v in &proof.verify {
            items.push((if v.ok { p.finished } else { p.failed }, v.command.clone()));
        }
        for tr in &proof.tried {
            items.push((if tr.ok { p.finished } else { p.failed }, tr.title.clone()));
        }
        for f in proof.standing().filter(|f| f.severity != tessel_proto::proof::Severity::Note).take(4) {
            items.push((p.blocked, f.title.clone()));
        }
        if items.is_empty() {
            return None;
        }
        let mut row = h_flex().w_full().flex_wrap().gap_x(theme::z(18.)).gap_y(theme::z(6.));
        for (color, text) in items {
            row = row.child(h_flex().gap(theme::z(8.)).items_center().child(div().size(theme::z(6.)).rounded_full().bg(color)).child(div().text_size(theme::z(13.)).text_color(if color == p.finished { p.secondary } else { color }).child(text)));
        }
        Some(row.into_any_element())
    }

    fn stage_evidence(&self, t: &Task, p: &Palette) -> Option<AnyElement> {
        let proof = t.session.and_then(|s| self.model.sessions.iter().find(|x| x.id == s)).and_then(|s| s.proof.clone());
        if t.changes.is_empty() && proof.is_none() {
            return None;
        }
        let label = |text: &str| div().pb(theme::z(8.)).text_size(theme::z(13.)).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(text.to_owned());
        let line = |text: String, color: Hsla| div().text_size(theme::z(13.)).line_height(theme::z(20.)).text_color(color).child(text);
        let check = |ok: bool, text: String| h_flex().gap(theme::z(10.)).items_center().child(div().size(theme::z(6.)).rounded_full().flex_shrink_0().bg(if ok { p.finished } else { p.failed })).child(div().flex_1().min_w_0().truncate().text_size(theme::z(13.)).line_height(theme::z(20.)).text_color(if ok { p.secondary } else { p.failed }).child(text));
        let mut col = v_flex().w_full().gap(theme::z(4.));
        if !t.changes.is_empty() {
            col = col.child(label("Changes"));
            for f in t.changes.iter().take(8) {
                col = col.child(
                    h_flex()
                        .gap(theme::z(8.))
                        .text_size(theme::z(12.5))
                        .line_height(theme::z(20.))
                        .child(div().flex_1().min_w_0().truncate().font_family(theme::mono()).text_color(p.label).child(f.path.clone()))
                        .child(div().font_features(theme::tabular()).text_color(p.finished).child(if f.added > 0 { format!("+{}", f.added) } else { String::new() }))
                        .child(div().font_features(theme::tabular()).text_color(p.failed).child(if f.removed > 0 { format!("−{}", f.removed) } else { String::new() })),
                );
            }
            if t.changes.len() > 8 {
                col = col.child(line(format!("and {} more", t.changes.len() - 8), p.tertiary));
            }
        }
        if let Some(proof) = proof {
            col = col.child(div().h(theme::z(14.))).child(label("Checks"));
            let mut any = false;
            for v in proof.verify.iter() {
                any = true;
                col = col.child(check(v.ok, v.command.clone()));
            }
            for tr in proof.tried.iter() {
                any = true;
                col = col.child(check(tr.ok, tr.title.clone()));
            }
            for f in proof.standing().filter(|f| f.severity != tessel_proto::proof::Severity::Note).take(4) {
                any = true;
                col = col.child(h_flex().gap(theme::z(10.)).items_center().child(div().size(theme::z(6.)).rounded_full().flex_shrink_0().bg(p.blocked)).child(div().flex_1().min_w_0().text_size(theme::z(13.)).line_height(theme::z(20.)).text_color(p.blocked).child(f.title.clone())));
            }
            if !any {
                col = col.child(line("The reviewers found nothing".into(), p.secondary));
            }
        }
        Some(col.into_any_element())
    }

    fn stage_answers(&self, t: &Task, shown: usize, swatched: bool, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        let id = t.id.clone();
        let mut row = h_flex().w_full().flex_wrap().gap(theme::z(12.)).items_end();
        let ask = |j: String, label: &'static str, kind: theme::ButtonKind, cx: &mut Context<Self>| {
            stage_button(&p, SharedString::from(format!("task-change-{j}")), label, kind, false, cx.listener(move |this, _, window, cx| {
                this.changing_task = Some(j.clone());
                this.task_change.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .into_any_element()
        };
        match t.state {
            TaskState::Review if swatched => {
                for (i, o) in t.options.iter().enumerate() {
                    let on = i == shown;
                    let thumb = o.shots.first().and_then(|s| self.shot_src(s));
                    row = row.child(
                        v_flex()
                            .id(("stage-swatch", i))
                            .test_support()
                            .aria_label(SharedString::from(o.label.clone()))
                            .w(theme::z(176.))
                            .p(theme::z(6.))
                            .gap(theme::z(8.))
                            .rounded(theme::z(22.))
                            .border_2()
                            .border_color(if on { p.accent } else { p.separator })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.stage_swatch = Some(i);
                                this.stage_shot = 0;
                                cx.notify();
                            }))
                            .child(div().w_full().h(theme::z(96.)).rounded(theme::z(theme::nested(22., 6.))).overflow_hidden().bg(p.fill).when_some(thumb, |d, src| d.child(img(src).size_full().object_fit(ObjectFit::Cover))))
                            .child(div().px(theme::z(6.)).pb(theme::z(4.)).text_size(theme::z(14.)).font_weight(FontWeight::MEDIUM).text_color(if on { p.label } else { p.secondary }).truncate().child(o.label.clone())),
                    );
                }
                let label = format!("Choose {}", t.options[shown].label);
                let j = id.clone();
                row = row
                    .child(div().w(theme::z(12.)))
                    .child(stage_button(&p, SharedString::from(format!("task-choose-{id}")), &label, theme::ButtonKind::Primary, false, cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Choose { index: shown }, cx))))
                    .child(ask(id.clone(), "Something Else", theme::ButtonKind::Ghost, cx));
            }
            TaskState::Review if !t.options.is_empty() => {
                for (i, o) in t.options.iter().enumerate() {
                    let j = id.clone();
                    row = row.child(stage_button(&p, SharedString::from(format!("task-choose-{id}-{i}")), &o.label, if i == 0 { theme::ButtonKind::Primary } else { theme::ButtonKind::Secondary }, false, cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Choose { index: i }, cx))));
                }
                row = row.child(ask(id.clone(), "Something Else", theme::ButtonKind::Ghost, cx));
            }
            TaskState::Review => {
                let i = id.clone();
                row = row
                    .child(stage_button(&p, SharedString::from(format!("task-land-{id}")), "Land", theme::ButtonKind::Primary, false, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Land, cx))))
                    .child(ask(id.clone(), "Ask for Changes", theme::ButtonKind::Secondary, cx));
            }
            TaskState::Draft => {
                let (i, j) = (id.clone(), id.clone());
                row = row
                    .child(stage_button(&p, SharedString::from(format!("task-ready-{id}")), "Start", theme::ButtonKind::Primary, false, cx.listener(move |this, _, _, cx| this.act_task(&i, TaskAct::Ready, cx))))
                    .child(stage_button(&p, SharedString::from(format!("task-plan-{id}")), "Plan", theme::ButtonKind::Secondary, false, cx.listener(move |this, _, _, cx| this.act_task(&j, TaskAct::Plan, cx))));
            }
            _ => return None,
        }
        Some(row.into_any_element())
    }

    fn stage_side_actions(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let id = t.id.clone();
        let small = |id: SharedString, label: &str, on_click: Click| {
            theme::button(&p, id, label.to_owned(), None, theme::ButtonKind::Ghost).h(theme::z(32.)).px(theme::z(12.)).text_size(theme::z(14.)).rounded(theme::z(16.)).text_color(p.secondary).on_click(on_click).test_support()
        };
        let mut row = h_flex().flex_shrink_0().gap(theme::z(2.)).items_center();
        if let Some(url) = t.pr.clone() {
            row = row.child(small(SharedString::from(format!("task-pr-{id}")), "Pull Request", Box::new(move |_, _, cx| cx.open_url(&url))));
        }
        if let Some(url) = t.source.clone() {
            row = row.child(small(SharedString::from(format!("task-source-{id}")), if url.contains("slack.com") { "Slack" } else { "Source" }, Box::new(move |_, _, cx| cx.open_url(&url))));
        }
        if let Some(url) = t.issue.clone() {
            let label = url.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "Issue".to_owned(), |n| format!("Issue #{n}"));
            row = row.child(small(SharedString::from(format!("task-issue-{id}")), &label, Box::new(move |_, _, cx| cx.open_url(&url))));
        }
        if let Some(s) = t.session {
            row = row.child(small(SharedString::from(format!("task-chat-{id}")), "Open Chat", Box::new(cx.listener(move |this, _, _, cx| {
                this.factory_view = false;
                this.switch_open(s, false, cx);
                cx.notify();
            }))));
        }
        if !t.state.finished() {
            let i = id.clone();
            row = row.child(small(SharedString::from(format!("task-drop-{id}")), "Drop", Box::new(cx.listener(move |this, _, _, cx| {
                this.act_task(&i, TaskAct::Drop, cx);
                this.stage_pick_now(None);
            }))));
        }
        row.into_any_element()
    }
}

fn stage_button(p: &Palette, id: impl Into<SharedString>, label: &str, kind: theme::ButtonKind, wide: bool, on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static) -> impl IntoElement {
    theme::button(p, id, label.to_owned(), None, kind).h(theme::z(44.)).px(theme::z(18.)).text_size(theme::z(15.)).rounded(theme::z(22.)).when(wide, |d| d.w_full().justify_center()).on_click(on_click).test_support()
}
