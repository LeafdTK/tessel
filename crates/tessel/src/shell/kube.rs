//! the kubernetes module: a cluster at a glance, small enough for a pane.
//!
//! a context and a namespace, a kind (pods, deployments, services, nodes), a
//! filter, and the objects as rows: a dot for how each is doing, its name,
//! what's wrong when something is, and the numbers that matter (ready,
//! restarts, cpu, memory, age). open one for its facts and containers, and
//! what can be done: its logs or a shell in a terminal, a restart, a delete.
//!
//! it all goes through the daemon, which asks the person's own kubectl.
//! nothing is asked while no kubernetes pane is on screen.

use super::*;
use crate::theme::{ButtonKind, z};
use crate::ui::Module;
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use tessel_proto::kube::{KubeAct, KubeHealth, KubeKind, KubeObject};
use gpui_kit::uniform_list;
use std::cell::RefCell;
use std::rc::Rc;
use tessel_proto::{Request, Response};

/// how often a visible pane asks again, at most. a big cluster takes a while
/// to list (a thousand pods: seconds), so it's asked no more than a third of
/// the time: three times as long as the last answer took.
const EVERY: Duration = Duration::from_secs(4);

pub(crate) struct KubeView {
    contexts: Vec<String>,
    /// the kubeconfig's current context.
    current: Option<String>,
    /// the one picked here; none: the current one.
    context: Option<String>,
    namespaces: Vec<String>,
    /// none: every namespace.
    namespace: Option<String>,
    kind: KubeKind,
    objects: Vec<KubeObject>,
    metrics: bool,
    asked_contexts: bool,
    asked_namespaces: bool,
    /// what the list being fetched is of; a pick made meanwhile asks again.
    asking: Option<(Option<String>, Option<String>, KubeKind)>,
    asked_at: Option<Instant>,
    /// how long to wait between lists, from how long the last took.
    every: Duration,
    fetched: Option<Instant>,
    /// the first answer for what's picked hasn't come yet.
    loading: bool,
    error: Option<String>,
    /// what the last action said.
    said: Option<String>,
    /// the object opened: its namespace and name.
    open: Option<(Option<String>, String)>,
    /// asked to delete this one once already: the next click does.
    confirm: Option<String>,
    /// bumped whenever `objects` changes, so the lines are made again.
    epoch: u64,
    lines: RefCell<((u64, String, bool), Rc<Vec<Line>>)>,
    pub(crate) filter: Entity<InputState>,
}

impl KubeView {
    pub(crate) fn new(filter: Entity<InputState>) -> Self {
        Self {
            contexts: Vec::new(),
            current: None,
            context: None,
            namespaces: Vec::new(),
            namespace: None,
            kind: KubeKind::Pods,
            objects: Vec::new(),
            metrics: false,
            asked_contexts: false,
            asked_namespaces: false,
            asking: None,
            asked_at: None,
            every: EVERY,
            fetched: None,
            loading: true,
            error: None,
            said: None,
            open: None,
            confirm: None,
            epoch: 0,
            lines: RefCell::new(((u64::MAX, String::new(), false), Rc::default())),
            filter,
        }
    }

    fn pick(&self) -> (Option<String>, Option<String>, KubeKind) {
        (self.context.clone(), self.namespace.clone().filter(|_| self.kind.namespaced()), self.kind)
    }

    /// what's picked changed: show nothing old as if it were new.
    fn repick(&mut self) {
        self.objects.clear();
        self.epoch += 1;
        self.fetched = None;
        self.loading = true;
        self.error = None;
        self.open = None;
        self.confirm = None;
    }
}

/// a short age: 45s, 12m, 3h, 25h, 4d.
fn age(ms: u64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..60 => format!("{s}s"),
        60..3600 => format!("{}m", s / 60),
        3600..172_800 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn cpu(m: u64) -> String {
    if m >= 1000 { format!("{:.1}", m as f64 / 1000.0) } else { format!("{m}m") }
}

fn mem(b: u64) -> String {
    const MI: f64 = 1_048_576.0;
    let mi = b as f64 / MI;
    if mi >= 1024.0 { format!("{:.1}Gi", mi / 1024.0) } else { format!("{mi:.0}Mi") }
}

/// what's worth saying beside the name: anything but the usual good word.
fn trouble(o: &KubeObject) -> Option<&str> {
    (!matches!(o.status.as_str(), "Running" | "Available" | "Ready" | "ClusterIP")).then_some(o.status.as_str())
}

/// the objects the filter lets through: every word somewhere in the name, namespace or status.
fn matching<'a>(objects: &'a [KubeObject], filter: &str) -> Vec<&'a KubeObject> {
    let words: Vec<String> = filter.split_whitespace().map(str::to_lowercase).collect();
    objects
        .iter()
        .filter(|o| {
            let hay = format!("{} {} {}", o.name, o.namespace.as_deref().unwrap_or(""), o.status).to_lowercase();
            words.iter().all(|w| hay.contains(w.as_str()))
        })
        .collect()
}

impl Shell {
    pub(crate) fn kube_visible(&self) -> bool {
        self.ui.panes().into_iter().any(|p| p.module == Some(Module::Kube) && self.pane_open(p))
    }

    fn kube_ask(&mut self, tag: &'static str, req: Request) {
        self.cmd(Cmd::Ask { tag, req });
    }

    /// every second: what a visible pane needs, and the list again when it's due.
    pub(crate) fn kube_tick(&mut self) {
        if !self.kube_visible() {
            return;
        }
        if !self.kube.asked_contexts {
            self.kube.asked_contexts = true;
            self.kube_ask("kube.contexts", Request::KubeContexts);
        }
        if !self.kube.asked_namespaces {
            self.kube.asked_namespaces = true;
            self.kube_ask("kube.namespaces", Request::KubeNamespaces { context: self.kube.context.clone() });
        }
        let due = self.kube.fetched.is_none_or(|t| t.elapsed() >= self.kube.every);
        if self.kube.asking.is_none() && due {
            self.kube_fetch();
        }
    }

    fn kube_fetch(&mut self) {
        let pick = self.kube.pick();
        self.kube.asking = Some(pick.clone());
        self.kube.asked_at = Some(Instant::now());
        let (context, namespace, kind) = pick;
        self.kube_ask("kube.list", Request::KubeList { context, namespace, kind });
    }

    pub(crate) fn kube_answer(&mut self, tag: &'static str, result: Result<Response, String>, cx: &mut Context<Self>) {
        match (tag, result) {
            ("kube.contexts", Ok(Response::KubeContexts { current, contexts })) => {
                self.kube.current = current;
                self.kube.contexts = contexts;
            }
            ("kube.namespaces", Ok(Response::KubeNamespaces { namespaces })) => self.kube.namespaces = namespaces,
            ("kube.namespaces", Err(_)) => {}
            ("kube.list", result) => {
                let asked = self.kube.asking.take();
                // picked something else meanwhile: this is old news; ask for what's picked.
                if asked.as_ref() != Some(&self.kube.pick()) {
                    if self.kube_visible() {
                        self.kube_fetch();
                    }
                    return;
                }
                self.kube.fetched = Some(Instant::now());
                self.kube.every = self.kube.asked_at.map_or(EVERY, |t| (t.elapsed() * 3).max(EVERY));
                self.kube.loading = false;
                match result {
                    Ok(Response::KubeObjects { objects, metrics, .. }) => {
                        self.kube.objects = objects;
                        self.kube.epoch += 1;
                        self.kube.metrics = metrics;
                        self.kube.error = None;
                    }
                    Ok(_) => {}
                    Err(e) => self.kube.error = Some(e),
                }
            }
            ("kube.act", Ok(Response::Shipped { said, .. })) => {
                self.kube.said = Some(said);
                self.kube.fetched = None;
            }
            ("kube.act", Err(e)) => self.kube.said = Some(e),
            ("kube.contexts", Err(e)) => self.kube.error = Some(e),
            _ => {}
        }
        cx.notify();
    }

    fn kube_set_context(&mut self, context: String, cx: &mut Context<Self>) {
        let context = (self.kube.current.as_ref() != Some(&context)).then_some(context);
        if context != self.kube.context {
            self.kube.context = context;
            self.kube.namespaces.clear();
            self.kube.asked_namespaces = false;
            self.kube.namespace = None;
            self.kube.repick();
            self.kube_tick();
        }
        cx.notify();
    }

    fn kube_set_namespace(&mut self, namespace: Option<String>, cx: &mut Context<Self>) {
        if namespace != self.kube.namespace {
            self.kube.namespace = namespace;
            self.kube.repick();
            self.kube_tick();
        }
        cx.notify();
    }

    fn kube_set_kind(&mut self, kind: KubeKind, cx: &mut Context<Self>) {
        if kind != self.kube.kind {
            self.kube.kind = kind;
            self.kube.repick();
            self.kube_tick();
        }
        cx.notify();
    }

    /// `kubectl` with the picked context, then `rest`, as one shell line.
    fn kubectl_line(&self, rest: &[&str]) -> String {
        let quote = |s: &str| if s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c)) { s.to_owned() } else { format!("'{}'", s.replace('\'', "'\\''")) };
        let mut words = vec!["kubectl".to_owned()];
        if let Some(c) = &self.kube.context {
            words.extend(["--context".to_owned(), quote(c)]);
        }
        words.extend(rest.iter().map(|w| quote(w)));
        words.join(" ")
    }

    fn kube_logs(&mut self, ns: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.kubectl_line(&["logs", "-f", "--tail=200", "--all-containers", "--prefix", "-n", ns, name]);
        self.run_in_terminal(line, Module::Kube, window, cx);
    }

    fn kube_shell(&mut self, ns: &str, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let line = self.kubectl_line(&["exec", "-it", "-n", ns, name, "--", "sh", "-c", "command -v bash >/dev/null && exec bash || exec sh"]);
        self.run_in_terminal(line, Module::Kube, window, cx);
    }

    fn kube_act(&mut self, ns: String, name: String, act: KubeAct, cx: &mut Context<Self>) {
        self.kube.confirm = None;
        self.kube.said = None;
        let (context, kind) = (self.kube.context.clone(), self.kube.kind);
        self.kube_ask("kube.act", Request::KubeAct { context, namespace: ns, kind, name, act });
        cx.notify();
    }

    // ---- drawing --------------------------------------------------------------

    /// the rows to show: the filter's, under a header per namespace when every
    /// namespace is shown. made again only when the list or the filter changes.
    fn kube_lines(&self, filter: &str) -> Rc<Vec<Line>> {
        let all_ns = self.kube.namespace.is_none() && self.kube.kind.namespaced();
        let key = (self.kube.epoch, filter.to_owned(), all_ns);
        let mut cache = self.kube.lines.borrow_mut();
        if cache.0 != key {
            let mut lines = Vec::new();
            let mut last: Option<&str> = None;
            let shown = matching(&self.kube.objects, filter);
            for (i, o) in shown.iter().enumerate() {
                let ns = o.namespace.as_deref().unwrap_or("");
                if all_ns && last != Some(ns) {
                    let n = shown[i..].iter().take_while(|x| x.namespace.as_deref().unwrap_or("") == ns).count();
                    lines.push(Line::Head(ns.to_owned(), n));
                    last = Some(ns);
                }
                lines.push(Line::Row(Rc::new((*o).clone())));
            }
            *cache = (key, Rc::new(lines));
        }
        cache.1.clone()
    }

    pub(crate) fn kube_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let filter = self.kube.filter.read(cx).value().to_string();
        let lines = self.kube_lines(&filter);
        let body: AnyElement = if let Some(e) = self.kube.error.as_ref().filter(|_| self.kube.objects.is_empty()) {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(z(8.))
                .p(z(24.))
                .child(Icon::empty().path("icons/ship-wheel.svg").size(z(20.)).text_color(p.tertiary))
                .child(div().set(Type::Callout).text_color(p.secondary).text_center().child(e.clone()))
                .into_any_element()
        } else if self.kube.loading {
            v_flex().flex_1().items_center().justify_center().child(div().set(Type::Callout).text_color(p.tertiary).child("Asking the cluster…")).into_any_element()
        } else if lines.is_empty() {
            let what = if filter.trim().is_empty() { format!("No {} here.", self.kube.kind.title().to_lowercase()) } else { format!("No {} match “{}”.", self.kube.kind.title().to_lowercase(), filter.trim()) };
            v_flex().flex_1().items_center().justify_center().child(div().set(Type::Callout).text_color(p.tertiary).child(what)).into_any_element()
        } else {
            let me = cx.entity().downgrade();
            let look = Look { p, metrics: self.kube.metrics, kind: self.kube.kind, open: self.kube.open.clone() };
            let count = lines.len();
            uniform_list("kube-rows", count, move |range, _, _| range.map(|i| line_el(&lines[i], &look, &me)).collect())
                .flex_1()
                .min_h_0()
                .py(z(4.))
                .into_any_element()
        };
        let open = self.kube.open.as_ref().and_then(|(ns, name)| self.kube.objects.iter().find(|o| &o.namespace == ns && &o.name == name));
        v_flex()
            .size_full()
            .child(self.kube_bar(cx))
            .child(div().h(px(1.)).flex_shrink_0().bg(p.separator))
            .children(self.kube.error.as_ref().filter(|_| !self.kube.objects.is_empty()).map(|e| {
                div().px(z(12.)).py(z(6.)).set(Type::Subheadline).text_color(p.failed).child(format!("Couldn't refresh: {e}"))
            }))
            .children(self.kube.said.as_ref().map(|s| {
                h_flex()
                    .px(z(12.))
                    .py(z(6.))
                    .gap(z(6.))
                    .set(Type::Subheadline)
                    .text_color(p.secondary)
                    .child(div().flex_1().min_w_0().truncate().child(s.clone()))
                    .child(div().id("kube-said-close").cursor_pointer().text_color(p.tertiary).child("✕").on_click(cx.listener(|this, _, _, cx| {
                        this.kube.said = None;
                        cx.notify();
                    })))
            }))
            .child(body)
            .children(open.map(|o| {
                v_flex()
                    .id("kube-detail")
                    .flex_shrink_0()
                    .max_h(relative(0.5))
                    .overflow_y_scroll()
                    .border_t_1()
                    .border_color(p.separator)
                    .child(self.kube_detail(o, cx))
            }))
            .into_any_element()
    }

    /// context and namespace, the kind, and the filter.
    fn kube_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let context = self.kube.context.clone().or_else(|| self.kube.current.clone()).unwrap_or_else(|| "kubectl".into());
        let namespace = self.kube.namespace.clone().unwrap_or_else(|| "All namespaces".into());
        let me = cx.entity().downgrade();
        let (contexts, current_ctx) = (self.kube.contexts.clone(), context.clone());
        let ctx_menu = Popover::new("kube-context-menu").p_0().anchor(Anchor::TopLeft).trigger(picker("kube-context", "ship-wheel", context.clone())).content({
            let me = me.clone();
            move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let p = shell.read(cx).palette.get();
                theme::float_list(&p)
                    .w(px(280.))
                    .children(contexts.iter().enumerate().map(|(i, c)| {
                        let (me, pop, c2) = (me.clone(), popover.clone(), c.clone());
                        theme::float_item(&p, SharedString::from(format!("kube-context-{i}")), None, c.clone(), None, *c == current_ctx).on_click(move |_, window, cx| {
                            pop.update(cx, |st, cx| st.dismiss(window, cx));
                            let _ = me.update(cx, |this, cx| this.kube_set_context(c2.clone(), cx));
                        })
                    }))
                    .into_any_element()
            }
        });
        let (namespaces, picked_ns) = (self.kube.namespaces.clone(), self.kube.namespace.clone());
        let ns_menu = Popover::new("kube-namespace-menu").p_0().anchor(Anchor::TopLeft).trigger(picker("kube-namespace", "box", namespace)).content({
            let me = me.clone();
            move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let p = shell.read(cx).palette.get();
                let choices = std::iter::once(None).chain(namespaces.iter().cloned().map(Some));
                theme::float_list(&p)
                    .w(px(240.))
                    .max_h(px(360.))
                    .children(choices.enumerate().map(|(i, ns)| {
                        let (me, pop, ns2) = (me.clone(), popover.clone(), ns.clone());
                        let name = ns.clone().unwrap_or_else(|| "All namespaces".into());
                        theme::float_item(&p, SharedString::from(format!("kube-namespace-{i}")), None, name, None, ns == picked_ns).on_click(move |_, window, cx| {
                            pop.update(cx, |st, cx| st.dismiss(window, cx));
                            let _ = me.update(cx, |this, cx| this.kube_set_namespace(ns2.clone(), cx));
                        })
                    }))
                    .into_any_element()
            }
        });
        let mut kinds = theme::tray(&p);
        for kind in KubeKind::ALL {
            kinds = kinds.child(theme::segment(&p, SharedString::from(format!("kube-kind-{}", kind.resource())), kind.title(), self.kube.kind == kind).test_support().on_click(cx.listener(move |this, _, _, cx| this.kube_set_kind(kind, cx))));
        }
        v_flex()
            .flex_shrink_0()
            .px(z(8.))
            .py(z(8.))
            .gap(z(6.))
            .child(
                h_flex()
                    .gap(z(4.))
                    .child(ctx_menu)
                    .when(self.kube.kind.namespaced(), |d| d.child(ns_menu))
                    .child(div().flex_1().min_w(z(60.)).child(Input::new(&self.kube.filter).id("kube-filter").small())),
            )
            .child(
                h_flex()
                    .gap(z(6.))
                    .child(kinds)
                    .child(div().flex_1())
                    .child(div().set(Type::Subheadline).text_color(p.tertiary).child(if self.kube.loading { String::new() } else { format!("{}", self.kube.objects.len()) })),
            )
    }

    /// an opened object: its facts, its containers, and what can be done to it.
    fn kube_detail(&self, o: &KubeObject, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let ns = o.namespace.clone().unwrap_or_default();
        let name = o.name.clone();
        let fact = |k: &str, v: &str| {
            h_flex()
                .gap(z(8.))
                .items_start()
                .child(div().w(z(84.)).flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(k.to_owned()))
                .child(div().flex_1().min_w_0().set(Type::Subheadline).text_color(p.label).child(v.to_owned()))
        };
        let mut actions = h_flex().gap(z(6.)).pt(z(4.));
        match self.kube.kind {
            KubeKind::Pods => {
                let (ns1, n1, ns2, n2, ns3, n3) = (ns.clone(), name.clone(), ns.clone(), name.clone(), ns.clone(), name.clone());
                actions = actions
                    .child(theme::button(&p, "kube-logs", "Logs", Some("scroll-text"), ButtonKind::Secondary).test_support().on_click(cx.listener(move |this, _, window, cx| this.kube_logs(&ns1, &n1, window, cx))))
                    .child(theme::button(&p, "kube-shell", "Shell", Some("square-terminal"), ButtonKind::Secondary).test_support().on_click(cx.listener(move |this, _, window, cx| this.kube_shell(&ns2, &n2, window, cx))))
                    .child(div().flex_1());
                let sure = self.kube.confirm.as_deref() == Some(name.as_str());
                actions = actions.child(
                    theme::button(&p, "kube-delete", if sure { "Delete it" } else { "Delete" }, Some("trash-2"), if sure { ButtonKind::Primary } else { ButtonKind::Ghost })
                        .test_support()
                        .when(!sure, |b| b.text_color(p.failed))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.kube.confirm.as_deref() == Some(n3.as_str()) {
                                this.kube_act(ns3.clone(), n3.clone(), KubeAct::Delete, cx);
                            } else {
                                this.kube.confirm = Some(n3.clone());
                                cx.notify();
                            }
                        })),
                );
            }
            KubeKind::Deployments => {
                let (ns1, n1) = (ns.clone(), name.clone());
                actions = actions.child(theme::button(&p, "kube-restart", "Restart", Some("rotate-cw"), ButtonKind::Secondary).test_support().on_click(cx.listener(move |this, _, _, cx| this.kube_act(ns1.clone(), n1.clone(), KubeAct::Restart, cx))));
            }
            _ => {}
        }
        let has_actions = matches!(self.kube.kind, KubeKind::Pods | KubeKind::Deployments);
        v_flex()
            .px(z(12.))
            .py(z(10.))
            .gap(z(4.))
            .child(
                h_flex()
                    .gap(z(8.))
                    .pb(z(2.))
                    .child(div().flex_1().min_w_0().truncate().set(Type::Headline).text_color(p.label).child(o.name.clone()))
                    .child(div().id("kube-detail-close").cursor_pointer().set(Type::Subheadline).text_color(p.tertiary).hover(|d| d.text_color(p.label)).child("✕").on_click(cx.listener(|this, _, _, cx| {
                        this.kube.open = None;
                        this.kube.confirm = None;
                        cx.notify();
                    }))),
            )
            .child(fact("Status", &o.status))
            .children(o.namespace.as_ref().map(|n| fact("Namespace", n)))
            .children(o.facts.iter().map(|(k, v)| fact(k, v)))
            .children(o.containers.iter().map(|c| {
                let dot = if c.ready { p.finished } else if c.state.starts_with("waiting") { p.blocked } else { p.failed };
                h_flex()
                    .gap(z(8.))
                    .pt(z(2.))
                    .child(div().size(z(6.)).flex_shrink_0().rounded_full().bg(dot))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex()
                                    .gap(z(6.))
                                    .set(Type::Subheadline)
                                    .child(div().text_color(p.label).font_weight(FontWeight::MEDIUM).child(c.name.clone()))
                                    .child(div().text_color(p.secondary).child(c.state.clone()))
                                    .when(c.restarts > 0, |d| d.child(div().text_color(p.blocked).child(format!("↻{}", c.restarts)))),
                            )
                            .child(div().min_w_0().truncate().set(Type::Code).text_color(p.tertiary).child(c.image.clone())),
                    )
            }))
            .when(has_actions, |d| d.child(actions))
    }
}

/// a line of the list: a namespace's header, or an object.
pub(crate) enum Line {
    Head(String, usize),
    Row(Rc<KubeObject>),
}

/// what a line needs to know to be drawn, outside the shell.
struct Look {
    p: Palette,
    metrics: bool,
    kind: KubeKind,
    open: Option<(Option<String>, String)>,
}

fn line_el(line: &Line, look: &Look, me: &WeakEntity<Shell>) -> AnyElement {
    let p = &look.p;
    let o = match line {
        Line::Head(ns, n) => {
            return h_flex()
                .w_full()
                .h(z(28.))
                .px(z(12.))
                .pt(z(6.))
                .gap(z(6.))
                .items_center()
                .set(Type::Subheadline)
                .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(ns.clone()))
                .child(div().flex_shrink_0().text_color(p.quaternary).child(n.to_string()))
                .into_any_element();
        }
        Line::Row(o) => o,
    };
    let dot = match o.health {
        KubeHealth::Ok => p.finished,
        KubeHealth::Pending => p.blocked,
        KubeHealth::Bad => p.failed,
        KubeHealth::Quiet => p.quaternary,
    };
    let key = (o.namespace.clone(), o.name.clone());
    let open = look.open.as_ref() == Some(&key);
    let id = SharedString::from(format!("kube-row-{}-{}", o.namespace.as_deref().unwrap_or(""), o.name));
    let small = |w: f32, s: String, ink: Hsla| div().w(z(w)).flex_shrink_0().text_right().set(Type::Subheadline).text_color(ink).child(s);
    let me = me.clone();
    let row = h_flex()
        .id(id)
        .test_support()
        .w_full()
        .h(z(28.))
        .px(z(8.))
        .gap(z(8.))
        .rounded(z(theme::R_TILE))
        .cursor_pointer()
        .when(open, |d| d.bg(p.selection))
        .when(!open, |d| d.hover(|d| d.bg(p.hover)))
        .on_click(move |_, _, cx| {
            let _ = me.update(cx, |this, cx| {
                this.kube.open = if this.kube.open.as_ref() == Some(&key) { None } else { Some(key.clone()) };
                this.kube.confirm = None;
                cx.notify();
            });
        })
        .child(div().size(z(7.)).flex_shrink_0().rounded_full().bg(dot))
        .child(
            h_flex()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .gap(z(6.))
                .set(Type::Body)
                .child(div().min_w_0().flex_shrink(1.0).truncate().text_color(p.label).child(o.name.clone()))
                .children(trouble(o).map(|t| {
                    let ink = if o.health == KubeHealth::Bad { p.failed } else { p.secondary };
                    div().flex_shrink_0().set(Type::Subheadline).text_color(ink).child(t.to_owned())
                })),
        )
        .children(o.restarts.filter(|r| *r > 0).map(|r| small(28., format!("↻{r}"), p.blocked)))
        .children(o.ready.clone().filter(|_| look.kind != KubeKind::Services).map(|r| small(30., r, p.secondary)))
        .when(look.metrics, |d| d.child(small(40., o.cpu_milli.map(cpu).unwrap_or_default(), p.secondary)).child(small(48., o.mem_bytes.map(mem).unwrap_or_default(), p.secondary)))
        .child(small(30., o.created_ms.map(age).unwrap_or_default(), p.tertiary));
    div().w_full().px(z(4.)).child(row).into_any_element()
}

/// a dropdown's face: an icon, what's picked, a chevron.
fn picker(id: &'static str, icon: &'static str, label: String) -> Button {
    Button::new(id).ghost().small().icon(Icon::empty().path(format!("icons/{icon}.svg"))).label(label).dropdown_caret(true)
}

#[cfg(test)]
mod tests {
    // not `super::*`: that brings gpui's `test` along.
    use super::{cpu, matching, mem, trouble};
    use tessel_proto::kube::KubeObject;

    #[test]
    fn the_filter_wants_every_word_somewhere() {
        let o = |name: &str, ns: &str, status: &str| KubeObject { name: name.into(), namespace: Some(ns.into()), status: status.into(), ..Default::default() };
        let objects = vec![o("server-1", "orchard", "Running"), o("server-2", "orchard", "CrashLoopBackOff"), o("redis", "cache", "Running")];
        let names = |f: &str| matching(&objects, f).iter().map(|o| o.name.clone()).collect::<Vec<_>>();
        assert_eq!(names("server crash"), ["server-2"]);
        assert_eq!(names("ORCHARD"), ["server-1", "server-2"]);
        assert_eq!(names(""), ["server-1", "server-2", "redis"]);
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!((cpu(250), cpu(1500)), ("250m".to_owned(), "1.5".to_owned()));
        assert_eq!((mem(985 * 1_048_576), mem(2 * 1_073_741_824)), ("985Mi".to_owned(), "2.0Gi".to_owned()));
        assert_eq!(trouble(&KubeObject { status: "Running".into(), ..Default::default() }), None);
        assert_eq!(trouble(&KubeObject { status: "CrashLoopBackOff".into(), ..Default::default() }), Some("CrashLoopBackOff"));
    }
}
