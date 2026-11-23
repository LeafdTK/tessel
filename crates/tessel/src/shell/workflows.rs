use super::*;
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::popover::Popover;
use std::collections::{BTreeMap, HashMap};
use serde_json::Value;
use tessel_proto::{Choice, InputKind, Pick, Workflow};

pub(crate) enum Loaded {
    Loading,
    Ready(Vec<Choice>),
    Failed(String),
}

pub(crate) struct WfForm {
    pub(crate) workflow: Workflow,
    pub(crate) cwd: Option<String>,
    pub(crate) picked: BTreeMap<String, Pick>,
    pub(crate) choices: HashMap<String, Loaded>,
    pub(crate) texts: HashMap<String, Entity<InputState>>,
    pub(crate) running: bool,
    pub(crate) error: Option<String>,
}

impl WfForm {
    fn needs(&self, input: &str) -> Vec<String> {
        self.workflow.inputs.iter().find(|i| i.id == input).and_then(|i| i.options.as_ref()).map(|o| o.needs()).unwrap_or_default()
    }

    fn ready(&self, input: &str) -> bool {
        self.needs(input).iter().all(|n| self.picked.contains_key(n))
    }

    fn after(&self, input: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut frontier = vec![input.to_owned()];
        while let Some(at) = frontier.pop() {
            for i in &self.workflow.inputs {
                if !out.contains(&i.id) && self.needs(&i.id).contains(&at) {
                    out.push(i.id.clone());
                    frontier.push(i.id.clone());
                }
            }
        }
        out
    }
}

pub(crate) fn matches(w: &Workflow, text: &str) -> bool {
    let text = text.trim().to_lowercase();
    if text.is_empty() || text.len() > 160 {
        return false;
    }
    std::iter::once(w.name.as_str()).chain(w.say.iter().map(String::as_str)).any(|phrase| {
        let phrase = phrase.trim().to_lowercase();
        !phrase.is_empty() && (text.contains(&phrase) || phrase.split_whitespace().count() > 1 && phrase.split_whitespace().all(|word| text.split(|c: char| !c.is_alphanumeric()).any(|t| t == word)))
    })
}

impl Shell {
    pub(crate) fn ask_workflows(&mut self) {
        let cwd = self.new_folder();
        self.cmd(Cmd::Ask { tag: "workflows", req: tessel_proto::Request::Workflows { cwd } });
    }

    pub(crate) fn workflows_answer(&mut self, tag: &str, result: Result<tessel_proto::Response, String>, cx: &mut Context<Self>) {
        match (tag, result) {
            ("workflows", Ok(tessel_proto::Response::Workflows { workflows })) => self.workflows = workflows,
            ("workflows", _) => self.workflows.clear(),
            ("workflow.run", Ok(tessel_proto::Response::Session { summary })) => {
                if let Some(f) = self.wf_form.take() {
                    self.wf_last.insert(f.workflow.id.clone(), f.picked);
                }
                self.model.update_session(summary.clone());
                self.reflow_sidebar();
                self.clear_new_prompt = true;
                self.open(summary.id, cx);
            }
            ("workflow.run", Err(e)) => {
                if let Some(f) = self.wf_form.as_mut() {
                    f.running = false;
                    f.error = Some(e);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    pub(crate) fn workflow_for(&self, text: &str) -> Option<&Workflow> {
        self.workflows.iter().filter(|w| w.problems.is_empty()).find(|w| matches(w, text))
    }

    pub(crate) fn open_workflow(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let w = self.workflows.iter().find(|w| w.id == id || w.name.eq_ignore_ascii_case(id)).cloned().ok_or_else(|| format!("there's no workflow {id}"))?;
        if !w.problems.is_empty() {
            return Err(format!("{} needs fixing first: {}", w.name, w.problems.join("; ")));
        }
        let picked = self.wf_last.get(&w.id).cloned().unwrap_or_default();
        let mut texts = HashMap::new();
        for i in &w.inputs {
            if matches!(i.kind, InputKind::Text | InputKind::Long | InputKind::Date) {
                let start = picked.get(&i.id).map(|p| crate::shell::workflows::plain(&p.value)).or_else(|| i.default.as_ref().map(crate::shell::workflows::plain)).unwrap_or_default();
                let hint = i.placeholder.clone().unwrap_or_else(|| if i.kind == InputKind::Date { "YYYY-MM-DD".into() } else { String::new() });
                let state = cx.new(|cx| {
                    let mut s = InputState::new(window, cx).placeholder(hint);
                    s.set_value(start, window, cx);
                    s
                });
                texts.insert(i.id.clone(), state);
            }
        }
        let mut form = WfForm { workflow: w, cwd: self.new_folder(), picked, choices: HashMap::new(), texts, running: false, error: None };
        let keep: Vec<String> = form.picked.keys().filter(|k| form.workflow.inputs.iter().any(|i| &i.id == *k)).cloned().collect();
        form.picked.retain(|k, _| keep.contains(k));
        self.wf_form = Some(form);
        let ids: Vec<String> = self.wf_form.as_ref().map(|f| f.workflow.inputs.iter().filter(|i| i.options.is_some()).map(|i| i.id.clone()).collect()).unwrap_or_default();
        for id in ids {
            self.load_choices(&id, false);
        }
        cx.notify();
        Ok(())
    }

    pub(crate) fn close_workflow(&mut self, cx: &mut Context<Self>) {
        self.wf_form = None;
        cx.notify();
    }

    fn load_choices(&mut self, input: &str, fresh: bool) {
        let Some(f) = self.wf_form.as_mut() else { return };
        if !f.ready(input) {
            f.choices.remove(input);
            return;
        }
        f.choices.insert(input.to_owned(), Loaded::Loading);
        let cmd = Cmd::WorkflowOptions { workflow: f.workflow.id.clone(), input: input.to_owned(), picked: f.picked.clone(), cwd: f.cwd.clone(), fresh };
        self.cmd(cmd);
    }

    pub(crate) fn choices_answer(&mut self, workflow: String, input: String, result: Result<Vec<Choice>, String>, cx: &mut Context<Self>) {
        let Some(f) = self.wf_form.as_mut().filter(|f| f.workflow.id == workflow) else { return };
        if !matches!(f.choices.get(&input), Some(Loaded::Loading)) {
            return;
        }
        match result {
            Ok(c) => {
                if let Some(p) = f.picked.get(&input)
                    && !c.iter().any(|x| Value::String(x.value.clone()) == p.value)
                    && !p.value.is_array()
                {
                    f.picked.remove(&input);
                }
                f.choices.insert(input, Loaded::Ready(c));
            }
            Err(e) => {
                f.choices.insert(input, Loaded::Failed(e));
            }
        }
        cx.notify();
    }

    pub(crate) fn pick_choice(&mut self, input: &str, choice: Choice, multi: bool, cx: &mut Context<Self>) {
        let Some(f) = self.wf_form.as_mut() else { return };
        if multi {
            let mut values: Vec<Value> = f.picked.get(input).and_then(|p| p.value.as_array().cloned()).unwrap_or_default();
            let mut labels: Vec<String> = f.picked.get(input).map(|p| p.label.split(", ").filter(|s| !s.is_empty()).map(str::to_owned).collect()).unwrap_or_default();
            let v = Value::String(choice.value.clone());
            match values.iter().position(|x| *x == v) {
                Some(at) => {
                    values.remove(at);
                    labels.retain(|l| *l != choice.label);
                }
                None => {
                    values.push(v);
                    labels.push(choice.label.clone());
                }
            }
            if values.is_empty() {
                f.picked.remove(input);
            } else {
                f.picked.insert(input.to_owned(), Pick { value: Value::Array(values), label: labels.join(", ") });
            }
        } else {
            f.picked.insert(input.to_owned(), Pick { value: Value::String(choice.value), label: choice.label });
        }
        f.error = None;
        let after = f.after(input);
        for a in &after {
            f.picked.remove(a);
            f.choices.remove(a);
        }
        for a in after {
            self.load_choices(&a, false);
        }
        cx.notify();
    }

    fn set_toggle(&mut self, input: &str, on: bool, cx: &mut Context<Self>) {
        if let Some(f) = self.wf_form.as_mut() {
            f.picked.insert(input.to_owned(), Pick { value: Value::Bool(on), label: if on { "yes".into() } else { "no".into() } });
        }
        cx.notify();
    }

    fn pick_file(&mut self, input: String, cx: &mut Context<Self>) {
        let pick = cx.prompt_for_paths(PathPromptOptions { files: true, directories: false, multiple: false, prompt: Some("Choose".into()) });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = pick.await else { return };
            let Some(path) = paths.into_iter().next() else { return };
            let _ = this.update(cx, |this, cx| {
                if let Some(f) = this.wf_form.as_mut() {
                    let label = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    f.picked.insert(input, Pick { value: Value::String(path.to_string_lossy().into_owned()), label });
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub(crate) fn run_workflow_form(&mut self, cx: &mut Context<Self>) {
        let Some(f) = self.wf_form.as_mut() else { return };
        if f.running {
            return;
        }
        for (id, state) in &f.texts {
            let text = state.read(cx).value().trim().to_string();
            if text.is_empty() {
                f.picked.remove(id);
            } else {
                f.picked.insert(id.clone(), Pick { value: Value::String(text.clone()), label: text });
            }
        }
        if let Some(missing) = f.workflow.inputs.iter().find(|i| !i.optional && i.kind != InputKind::Toggle && !f.picked.contains_key(&i.id)) {
            f.error = Some(format!("{} is needed", missing.label));
            cx.notify();
            return;
        }
        f.running = true;
        f.error = None;
        let req = tessel_proto::Request::WorkflowRun { workflow: f.workflow.id.clone(), picked: f.picked.clone(), cwd: f.cwd.clone() };
        self.cmd(Cmd::Ask { tag: "workflow.run", req });
        cx.notify();
    }

    pub(crate) fn workflow_cards(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        let ready: Vec<&Workflow> = self.workflows.iter().collect();
        if ready.is_empty() {
            return None;
        }
        let mut row = h_flex().flex_wrap().gap_2();
        for w in ready {
            let id = w.id.clone();
            let broken = !w.problems.is_empty();
            row = row.child(
                h_flex()
                    .id(SharedString::from(format!("workflow-card-{}", w.id)))
                    .test_support()
                    .gap_2()
                    .px_3()
                    .h(px(34.))
                    .rounded(px(theme::R_TILE))
                    .border_1()
                    .border_color(p.separator)
                    .when(!crate::glass::liquid(), |d| d.bg(p.raised))
                    .hover(|d| d.bg(p.hover))
                    .cursor_pointer()
                    .when(broken, |d| d.opacity(0.55))
                    .child(Icon::empty().path(format!("icons/{}.svg", w.icon.as_deref().unwrap_or("workflow"))).size(theme::z(14.)).text_color(p.secondary))
                    .child(div().set(Type::Callout).text_color(p.label).child(w.name.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Err(e) = this.open_workflow(&id, window, cx) {
                            this.error = Some(e);
                        }
                        cx.notify();
                    })),
            );
        }
        Some(v_flex().gap_2().child(div().px_1().set(Type::Subheadline).text_color(p.tertiary).child("Workflows")).child(row).into_any_element())
    }

    pub(crate) fn workflow_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let p = self.palette.get();
        let text = self.new_prompt.read(cx).value().to_string();
        let w = self.workflow_for(&text)?;
        let id = w.id.clone();
        Some(
            h_flex()
                .id("workflow-chip")
                .test_support()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(px(theme::R_TILE))
                .bg(p.accent.opacity(0.12))
                .text_color(p.accent)
                .cursor_pointer()
                .child(Icon::empty().path(format!("icons/{}.svg", w.icon.as_deref().unwrap_or("workflow"))).size(theme::z(13.)))
                .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).child(format!("Run {}", w.name)))
                .child(theme::keycaps(&p, "↵"))
                .child(div().set(Type::Subheadline).text_color(p.tertiary).child("⌘↵ just chats"))
                .on_click(cx.listener(move |this, _, window, cx| {
                    if let Err(e) = this.open_workflow(&id, window, cx) {
                        this.error = Some(e);
                    }
                    cx.notify();
                }))
                .into_any_element(),
        )
    }

    pub(crate) fn workflow_layer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(f) = self.wf_form.as_ref() else { return div().into_any_element() };
        let p = self.palette.get();
        let w = &f.workflow;
        let mut fields = v_flex().gap_4();
        for i in &w.inputs {
            let label = h_flex()
                .gap_1()
                .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).text_color(p.label).child(i.label.clone()))
                .when(i.optional, |d| d.child(div().set(Type::Subheadline).text_color(p.tertiary).child("optional")));
            let control: AnyElement = match i.kind {
                InputKind::Text | InputKind::Long | InputKind::Date => match f.texts.get(&i.id) {
                    Some(state) => div().id(SharedString::from(format!("wf-field-{}", i.id))).test_support().child(Input::new(state)).into_any_element(),
                    None => div().into_any_element(),
                },
                InputKind::Toggle => {
                    let id = i.id.clone();
                    let on = f.picked.get(&i.id).and_then(|p| p.value.as_bool()).or_else(|| i.default.as_ref().and_then(Value::as_bool)).unwrap_or(false);
                    gpui_kit::component::switch::Switch::new(SharedString::from(format!("wf-toggle-{}", i.id)))
                        .checked(on)
                        .on_click(cx.listener(move |this, on: &bool, _, cx| this.set_toggle(&id, *on, cx)))
                        .into_any_element()
                }
                InputKind::File => {
                    let id = i.id.clone();
                    let chosen = f.picked.get(&i.id).map(|p| p.label.clone());
                    theme::button(&p, SharedString::from(format!("wf-file-{}", i.id)), chosen.unwrap_or_else(|| "Choose a file…".into()), Some("file"), theme::ButtonKind::Secondary)
                        .test_support()
                        .on_click(cx.listener(move |this, _, _, cx| this.pick_file(id.clone(), cx)))
                        .into_any_element()
                }
                InputKind::Select | InputKind::Multiselect | InputKind::Unknown => self.choice_field(f, i, cx),
            };
            fields = fields.child(v_flex().gap_1p5().child(label).child(control));
        }
        let can_run = !f.running;
        div()
            .id("workflow-layer")
            .absolute()
            .inset_0()
            .occlude()
            .bg(gpui_kit::black().opacity(if p.dark { 0.45 } else { 0.18 }))
            .on_click(cx.listener(|this, _, _, cx| this.close_workflow(cx)))
            .flex()
            .items_center()
            .justify_center()
            .child(
                v_flex()
                    .id("workflow-sheet")
                    .test_support()
                    .aria_label(SharedString::from(w.name.clone()))
                    .w(px(520.))
                    .max_w(relative(0.92))
                    .max_h(relative(0.86))
                    .rounded(px(theme::R_PANEL))
                    .border_1()
                    .border_color(p.separator)
                    .when(!crate::glass::liquid(), |d| d.bg(p.raised).shadow_lg())
                    .when(crate::glass::liquid(), |d| d.relative().child(super::glass_float(theme::R_PANEL)))
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        h_flex()
                            .gap_3()
                            .px_5()
                            .pt_5()
                            .pb_3()
                            .child(
                                div()
                                    .size(px(36.))
                                    .flex_shrink_0()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(10.))
                                    .bg(p.accent.opacity(0.14))
                                    .child(Icon::empty().path(format!("icons/{}.svg", w.icon.as_deref().unwrap_or("workflow"))).size(theme::z(18.)).text_color(p.accent)),
                            )
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .child(div().set(Type::Title).text_color(p.label).child(w.name.clone()))
                                    .children(w.description.clone().map(|d| div().set(Type::Callout).text_color(p.secondary).child(d))),
                            ),
                    )
                    .child(div().id("workflow-fields").flex_1().min_h_0().overflow_y_scroll().px_5().py_3().child(fields))
                    .child(
                        h_flex()
                            .gap_2()
                            .px_5()
                            .py_4()
                            .border_t_1()
                            .border_color(p.separator)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .set(Type::Callout)
                                    .text_color(if f.error.is_some() { p.failed } else { p.tertiary })
                                    .child(f.error.clone().unwrap_or_else(|| format!("{} step{}", w.steps.len(), if w.steps.len() == 1 { "" } else { "s" }))),
                            )
                            .child(theme::button(&p, "workflow-cancel", "Cancel", None, theme::ButtonKind::Secondary).test_support().on_click(cx.listener(|this, _, _, cx| this.close_workflow(cx))))
                            .child(
                                theme::button(&p, "workflow-run", if f.running { "Starting…" } else { "Run" }, Some(if f.running { "loader-circle" } else { "play" }), theme::ButtonKind::Primary)
                                    .test_support()
                                    .when(!can_run, |b| b.opacity(0.7))
                                    .on_click(cx.listener(|this, _, _, cx| this.run_workflow_form(cx))),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn choice_field(&self, f: &WfForm, i: &tessel_proto::Input, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let multi = i.kind == InputKind::Multiselect;
        let id = i.id.clone();
        let waiting_on: Vec<String> = f.needs(&i.id).into_iter().filter(|n| !f.picked.contains_key(n)).collect();
        let chosen = f.picked.get(&i.id).map(|p| p.label.clone()).filter(|l| !l.is_empty());
        let me = cx.entity().downgrade();
        match f.choices.get(&i.id) {
            _ if !waiting_on.is_empty() => {
                let after: Vec<String> = waiting_on.iter().map(|n| f.workflow.inputs.iter().find(|x| &x.id == n).map_or_else(|| n.clone(), |x| x.label.to_lowercase())).collect();
                div().set(Type::Callout).text_color(p.tertiary).child(format!("Pick a {} first", after.join(" and a "))).into_any_element()
            }
            None | Some(Loaded::Loading) => h_flex()
                .id(SharedString::from(format!("wf-loading-{id}")))
                .test_support()
                .gap_2()
                .h(px(32.))
                .child(gpui_kit::component::spinner::Spinner::new().xsmall().color(p.tertiary))
                .child(div().set(Type::Callout).text_color(p.tertiary).child("Loading…"))
                .into_any_element(),
            Some(Loaded::Failed(e)) => {
                let again = id.clone();
                h_flex()
                    .gap_2()
                    .child(div().flex_1().min_w_0().set(Type::Callout).text_color(p.failed).child(e.clone()))
                    .child(theme::button(&p, SharedString::from(format!("wf-retry-{id}")), "Try again", Some("refresh-cw"), theme::ButtonKind::Secondary).on_click(cx.listener(move |this, _, _, cx| {
                        this.load_choices(&again, true);
                        cx.notify();
                    })))
                    .into_any_element()
            }
            Some(Loaded::Ready(choices)) if multi || choices.len() <= 4 && choices.iter().all(|c| c.label.chars().count() <= 18) => {
                let picked_values: Vec<Value> = match f.picked.get(&i.id).map(|p| &p.value) {
                    Some(Value::Array(a)) => a.clone(),
                    Some(v) => vec![v.clone()],
                    None => Vec::new(),
                };
                let mut row = h_flex().flex_wrap().gap_1p5();
                for (n, c) in choices.iter().enumerate() {
                    let on = picked_values.contains(&Value::String(c.value.clone()));
                    let (input, choice) = (id.clone(), c.clone());
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("wf-choice-{id}-{n}")))
                            .test_support()
                            .px_3()
                            .h(px(30.))
                            .flex()
                            .items_center()
                            .rounded(px(theme::R_TILE))
                            .border_1()
                            .border_color(if on { p.accent } else { p.separator })
                            .when(on, |d| d.bg(p.accent.opacity(0.12)).text_color(p.accent))
                            .when(!on, |d| d.text_color(p.label).hover(|d| d.bg(p.hover)))
                            .cursor_pointer()
                            .set(Type::Callout)
                            .child(c.label.clone())
                            .on_click(cx.listener(move |this, _, _, cx| this.pick_choice(&input, choice.clone(), multi, cx))),
                    );
                }
                if choices.is_empty() {
                    row = row.child(div().set(Type::Callout).text_color(p.tertiary).child("Nothing to pick from"));
                }
                row.into_any_element()
            }
            Some(Loaded::Ready(choices)) => {
                let list = choices.clone();
                let trigger_label = chosen.clone().unwrap_or_else(|| format!("Choose {}…", i.label.to_lowercase()));
                Popover::new(SharedString::from(format!("wf-select-{id}")))
                    .p_0()
                    .anchor(Anchor::TopLeft)
                    .trigger(
                        Button::new(SharedString::from(format!("wf-select-{id}-open")))
                            .outline()
                            .w_full()
                            .justify_start()
                            .child(
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(div().flex_1().min_w_0().truncate().text_color(if chosen.is_some() { p.label } else { p.tertiary }).child(trigger_label))
                                    .child(Icon::empty().path("icons/chevrons-up-down.svg").size(theme::z(13.)).text_color(p.tertiary)),
                            ),
                    )
                    .content(move |_, _, cx| {
                        let popover = cx.entity();
                        let Some(shell) = me.upgrade() else { return div().into_any_element() };
                        let p = shell.read(cx).palette.get();
                        let mut l = theme::float_list(&p).id(SharedString::from(format!("wf-select-{id}-list"))).w(px(440.)).max_h(px(340.)).overflow_y_scroll();
                        for (n, c) in list.iter().enumerate() {
                            let (me, popover, input, choice) = (me.clone(), popover.clone(), id.clone(), c.clone());
                            l = l.child(theme::float_item(&p, SharedString::from(format!("wf-option-{id}-{n}")), None, c.label.clone(), c.note.as_deref(), false).test_support().on_click(
                                move |_, window, cx| {
                                    popover.update(cx, |st, cx| st.dismiss(window, cx));
                                    let _ = me.update(cx, |this, cx| this.pick_choice(&input, choice.clone(), false, cx));
                                },
                            ));
                        }
                        l.into_any_element()
                    })
                    .into_any_element()
            }
        }
    }
}

impl Shell {
    pub(crate) fn open_run(&self) -> Option<(SessionId, &tessel_proto::Run)> {
        let s = self.model.open_summary()?;
        if let Some(r) = &s.run {
            return Some((s.id, r));
        }
        let parent = self.model.summary(s.parent?)?;
        parent.run.as_ref().map(|r| (parent.id, r))
    }

    pub(crate) fn run_updated(&mut self, summary: &SessionSummary) {
        let had = self.model.summary(summary.id).and_then(|s| s.run.as_ref()).and_then(|r| r.doc.clone());
        let now = summary.run.as_ref().and_then(|r| r.doc.clone());
        if had.is_none() && now.is_some() {
            let open = self.model.open_summary();
            if open.is_some_and(|o| o.id == summary.id || o.parent == Some(summary.id)) {
                self.doc_reveal = true;
            }
        }
    }

    pub(crate) fn doc_visible(&self) -> bool {
        self.ui.panes().into_iter().any(|p| p.module == Some(crate::ui::Module::Doc) && self.pane_open(p))
    }

    pub(crate) fn show_doc(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.doc_visible() {
            return;
        }
        let at = {
            let panes = self.ui.panes();
            let chat = self.chat_pane.clone();
            panes.iter().find(|p| Some(&p.id) == chat.as_ref()).or_else(|| panes.iter().find(|p| p.module == Some(crate::ui::Module::Conversation))).map(|p| p.trail.clone())
        };
        let Some(trail) = at else { return };
        let Some(id) = self.split_pane(trail, crate::ui::Axis::Row, window, cx) else { return };
        if let Some(t) = self.ui.panes().into_iter().find(|p| p.id == id).map(|p| p.trail.clone()) {
            self.set_pane_module(t, crate::ui::Module::Doc, window, cx);
        }
    }

    pub(crate) fn sync_doc(&mut self) {
        let wanted = self.open_run().and_then(|(head, r)| r.doc.clone().map(|d| (head, d)));
        let Some((head, path)) = wanted else { return };
        if self.doc_view.as_ref().is_some_and(|(p, _)| *p == path) || !self.doc_visible() {
            return;
        }
        self.doc_view = Some((path.clone(), None));
        let cwd = self.model.summary(head).map(|s| s.cwd.clone()).unwrap_or_default();
        let rel = std::path::Path::new(&path).strip_prefix(&cwd).map(|r| r.to_string_lossy().into_owned()).unwrap_or(path);
        self.cmd(Cmd::Ask { tag: "doc.read", req: tessel_proto::Request::FsRead { session: head, path: rel } });
    }

    pub(crate) fn run_strip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        use tessel_proto::StepState;
        let (_, run) = self.open_run()?;
        let p = self.palette.get();
        let now = now_ms();
        let secs = |ms: u64| {
            let s = ms / 1000;
            if s < 60 { format!("{s}s") } else { format!("{}m {}s", s / 60, s % 60) }
        };
        let mut row = h_flex().gap_1().flex_wrap().items_center();
        for (n, step) in run.steps.iter().enumerate() {
            if n > 0 {
                row = row.child(div().w(px(12.)).h(px(1.)).bg(p.separator));
            }
            let mark: AnyElement = match step.state {
                StepState::Working => status_dot(p.blocked, true, &p).into_any_element(),
                StepState::Asking => Icon::empty().path("icons/circle-question-mark.svg").size(theme::z(13.)).text_color(theme::info(&p)).into_any_element(),
                StepState::Done => Icon::empty().path("icons/circle-check.svg").size(theme::z(13.)).text_color(p.finished).into_any_element(),
                StepState::Failed => Icon::empty().path("icons/circle-x.svg").size(theme::z(13.)).text_color(p.failed).into_any_element(),
                _ => div().size(px(8.)).rounded_full().border_1().border_color(p.tertiary).into_any_element(),
            };
            let time = match (step.started_ms, step.finished_ms) {
                (Some(a), Some(b)) => Some(secs(b.saturating_sub(a))),
                (Some(a), None) if step.state == StepState::Working => Some(secs(now.saturating_sub(a))),
                _ => None,
            };
            let to = step.session;
            row = row.child(
                h_flex()
                    .id(SharedString::from(format!("run-step-{n}")))
                    .test_support()
                    .gap(px(6.))
                    .px_2()
                    .h(px(26.))
                    .rounded(px(theme::R_TILE))
                    .when(step.state == StepState::Asking, |d| d.bg(theme::info(&p).opacity(0.12)))
                    .hover(|d| d.bg(p.hover))
                    .child(div().size(px(14.)).flex().items_center().justify_center().child(mark))
                    .child(div().set(Type::Callout).text_color(if step.state == StepState::Waiting { p.tertiary } else { p.label }).child(match step.state {
                        StepState::Asking => format!("{}: waiting for you", step.name),
                        _ => step.name.clone(),
                    }))
                    .children(time.map(|t| div().set(Type::Subheadline).text_color(p.tertiary).child(t)))
                    .when_some(to, |d, to| d.cursor_pointer().on_click(cx.listener(move |this, _, _, cx| this.open(to, cx)))),
            );
        }
        let cost: f64 = run.steps.iter().map(|s| s.cost_usd).sum();
        let tail: AnyElement = if let Some(why) = &run.failed {
            div().set(Type::Callout).text_color(p.failed).child(why.clone()).into_any_element()
        } else if run.doc.is_some() {
            theme::button(&p, "run-open-doc", "Open doc", Some("file-text"), theme::ButtonKind::Secondary)
                .test_support()
                .on_click(cx.listener(|this, _, window, cx| this.show_doc(window, cx)))
                .into_any_element()
        } else {
            div().into_any_element()
        };
        Some(
            h_flex()
                .id("run-strip")
                .test_support()
                .flex_shrink_0()
                .gap_3()
                .px_3()
                .py_2()
                .border_b_1()
                .border_color(p.separator)
                .child(Icon::empty().path(format!("icons/{}.svg", run.icon.as_deref().unwrap_or("workflow"))).size(theme::z(14.)).text_color(p.secondary))
                .child(div().flex_1().min_w_0().child(row))
                .when(cost > 0.005, |d| d.child(div().set(Type::Subheadline).text_color(p.tertiary).child(format!("${cost:.2}"))))
                .child(tail)
                .into_any_element(),
        )
    }

    pub(crate) fn doc_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let quiet = |t: &str| v_flex().size_full().items_center().justify_center().set(Type::Callout).text_color(p.tertiary).child(t.to_owned()).into_any_element();
        let Some((_, run)) = self.open_run() else { return quiet("When a workflow finishes, its doc opens here.") };
        let Some(path) = run.doc.clone() else {
            return quiet(if run.done() { "This run didn't make a doc." } else { "The doc shows up here when the run gets to it." });
        };
        let text = self.doc_view.as_ref().filter(|(p, _)| *p == path).and_then(|(_, t)| t.clone());
        let Some(text) = text else { return quiet("Opening…") };
        let name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let (copy, save) = (text.clone(), text.clone());
        let local = !self.on_host();
        let reveal = path.clone();
        let header = h_flex()
            .h(px(40.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .border_b_1()
            .border_color(p.separator)
            .child(Icon::empty().path("icons/file-text.svg").size(theme::z(14.)).text_color(p.secondary))
            .child(div().flex_1().min_w_0().truncate().set(Type::Callout).text_color(p.label).child(format!("{} · {name}", run.name)))
            .child(Button::new("doc-copy").xsmall().ghost().icon(Icon::empty().path("icons/copy.svg")).label("Copy").on_click(move |_, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))))
            .child(Button::new("doc-save").xsmall().ghost().icon(Icon::empty().path("icons/download.svg")).label("Save as…").on_click(cx.listener(move |this, _, _, cx| this.save_doc(save.clone(), cx))))
            .when(local, |d| d.child(Button::new("doc-reveal").xsmall().ghost().icon(Icon::empty().path("icons/folder-open.svg")).tooltip("Show in Finder").on_click(move |_, _, cx| cx.reveal_path(std::path::Path::new(&reveal)))));
        v_flex().size_full().child(header).child(div().flex_1().min_h_0().child(self.document_el(text, cx))).into_any_element()
    }

    fn save_doc(&mut self, text: String, cx: &mut Context<Self>) {
        let dir = std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_default().join("Documents");
        let name = self.open_run().map(|(_, r)| format!("{}.md", crate::shell::workflows::file_slug(&r.name))).unwrap_or_else(|| "doc.md".into());
        let pick = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(path))) = pick.await else { return };
            let result = std::fs::write(&path, text);
            let _ = this.update(cx, |this, cx| {
                if let Err(e) = result {
                    this.error = Some(format!("Couldn't save it: {e}"));
                }
                cx.notify();
            });
        })
        .detach();
    }
}

pub(crate) fn file_slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_alphanumeric() {
            out.extend(c.to_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out = out.trim_matches('-').to_owned();
    if out.is_empty() { "doc".into() } else { out }
}

pub(crate) fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().map(plain).filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn what_you_type_finds_the_workflow_it_means() {
        let w = tessel_proto::Workflow { name: "Study guide".into(), say: vec!["help me study".into(), "exam prep".into()], ..Default::default() };
        for yes in ["create me a study guide", "Study Guide for calc", "can you help me study?", "exam prep please", "a guide to study"] {
            assert!(super::matches(&w, yes), "{yes}");
        }
        for no in ["", "study", "write a guide", "help me"] {
            assert!(!super::matches(&w, no), "{no}");
        }
    }
}
