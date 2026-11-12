//! panes: a tiled workspace, like a terminal multiplexer's.
//!
//! a `panes` node holds a split tree; each pane in it shows one module. panes
//! are laid out here, not by flexbox: the tree's shares become rectangles, and
//! each pane is drawn at a sprung copy of its rectangle. so whatever changes
//! the tree (a click, a drag, an agent editing `ui.json`), every pane glides
//! from where it was to where it now belongs, new panes grow in, closed ones
//! fade away.
//!
//! structure (splitting, closing, choosing a module, resizing) is written to
//! `ui.json`, so it lasts and so an agent sees what you did. whether a pane is
//! open or folded away is the window's own state, like a drawer's.

use super::*;
use crate::ui::{Axis, Module, Node, Pane, PaneTree, MODULES};
use gpui_kit::component::Icon;
use gpui_kit::component::popover::Popover;
use serde_json::{Value, json};

/// the gap between tiles: orchard's 8.
/// room between panes: islands float apart; compact panes sit a hairline apart.
pub(crate) fn gap(compact: bool) -> f32 {
    if compact { 1.0 } else { 8.0 }
}
/// the least a pane is given, side by side and stacked, while there is room for it.
const MIN_W: f32 = 280.0;
const MIN_H: f32 = 160.0;

/// split `room` by `shares` (which sum to 1, 0 for a folded child), giving
/// none less than `least` while there is room for that, and taking what they
/// need from the others in proportion.
fn fit(shares: Vec<f32>, room: f32, least: f32) -> Vec<f32> {
    let mut shares = shares;
    // too narrow for all of them: the last ones fold away until the rest fit.
    // (the first is kept whatever happens; it is usually the conversation.)
    loop {
        let open: Vec<usize> = (0..shares.len()).filter(|i| shares[*i] > 0.0).collect();
        if open.len() <= 1 || room + 0.5 >= least * open.len() as f32 {
            break;
        }
        let last = *open.last().expect("more than one is open");
        shares[last] = 0.0;
    }
    let sum: f32 = shares.iter().sum();
    if sum > 0.0 {
        shares.iter_mut().for_each(|s| *s /= sum);
    }
    let mut lens: Vec<f32> = shares.iter().map(|s| room * s).collect();
    let open = shares.iter().filter(|s| **s > 0.0).count();
    if open == 0 || room < least * open as f32 {
        return lens;
    }
    let mut fixed = vec![false; lens.len()];
    for _ in 0..open {
        let short: Vec<usize> = (0..lens.len()).filter(|i| shares[*i] > 0.0 && !fixed[*i] && lens[*i] < least).collect();
        if short.is_empty() {
            break;
        }
        for i in short {
            fixed[i] = true;
            lens[i] = least;
        }
        let taken: f32 = (0..lens.len()).filter(|i| fixed[*i]).map(|i| lens[i]).sum();
        let free: f32 = (0..lens.len()).filter(|i| !fixed[*i] && shares[*i] > 0.0).map(|i| shares[i]).sum();
        for i in 0..lens.len() {
            if !fixed[i] && shares[i] > 0.0 {
                lens[i] = (room - taken) * shares[i] / free.max(0.0001);
            }
        }
    }
    lens
}
const HEADER_H: f32 = 36.0;

/// a pane's place, as it is drawn now and where it is heading.
pub(crate) struct PaneFx {
    rect: [Spring; 4],
    alpha: Spring,
    module: Option<Module>,
    /// gone from the tree: still drawn while it fades.
    leaving: bool,
}

/// a gap between two tiles being dragged.
pub(crate) struct PaneDrag {
    /// the split, as child indices from the top of the layout.
    trail: Vec<usize>,
    /// the first of the two neighbours.
    index: usize,
    axis: Axis,
    start: f32,
    /// their shares when the drag began, and the room they share in points.
    shares: (f32, f32),
    room: f32,
}

/// where the gap between two neighbours is, to grab.
struct Gap {
    trail: Vec<usize>,
    index: usize,
    axis: Axis,
    rect: Rect,
    shares: (f32, f32),
    room: f32,
}

impl Shell {
    /// whether a pane is showing. open unless folded away.
    pub(crate) fn pane_open(&self, p: &Pane) -> bool {
        self.pane_folded.get(&p.id).map_or(p.open, |folded| !folded)
    }

    pub(crate) fn has_pane(&self, id: &str) -> bool {
        self.ui.panes().iter().any(|p| p.id == id)
    }

    pub(crate) fn toggle_pane(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(open) = self.ui.panes().into_iter().find(|p| p.id == id).map(|p| self.pane_open(p)) else { return };
        self.pane_folded.insert(id.to_owned(), open);
        if !open {
            self.refresh_workspace();
        }
        cx.notify();
    }

    /// the tabs of the workspace that are on screen, so worth asking the daemon about.
    pub(crate) fn visible_tabs(&self) -> Vec<Tab> {
        let mut tabs = Vec::new();
        for p in self.ui.panes() {
            if !self.pane_open(p) {
                continue;
            }
            match p.module {
                Some(Module::Workspace) => tabs.push(self.ws.tab),
                Some(Module::Changes) => tabs.push(Tab::Changes),
                Some(Module::Files) => tabs.push(Tab::Files),
                Some(Module::Checks) => tabs.push(Tab::Checks),
                Some(Module::Artifacts) => tabs.push(Tab::Artifacts),
                Some(Module::Info) => tabs.push(Tab::Info),
                _ => {}
            }
        }
        // a panel component in a drawer, as layouts without panes have.
        if self.has_panel && self.panel_drawer.as_deref().is_none_or(|id| self.drawer(id).is_some_and(|d| d.open)) {
            tabs.push(self.ws.tab);
        }
        tabs.sort_by_key(|t| *t as u8);
        tabs.dedup();
        tabs
    }

    // ---- laying out ----------------------------------------------------------------

    /// each open pane's rectangle in `area`, and the gaps between neighbours.
    fn layout(&self, tree: &PaneTree, area: Rect, out: &mut Vec<(String, Rect)>, gaps: &mut Vec<Gap>) {
        match tree {
            PaneTree::Pane(p) => out.push((p.id.clone(), area)),
            PaneTree::Split { axis, children, trail, .. } => {
                let shown: Vec<(usize, &PaneTree)> = children.iter().enumerate().filter(|(_, c)| self.any_open(c)).collect();
                // a folded child keeps a place of no size, so it unfolds from where it lives.
                let total: f32 = shown.iter().map(|(_, c)| c.size()).sum::<f32>().max(0.0001);
                let along = if *axis == Axis::Row { area.w } else { area.h };
                let gap = gap(self.ui.theme.compact.unwrap_or(false));
                let room = (along - gap * shown.len().saturating_sub(1) as f32).max(0.0);
                let mut at = if *axis == Axis::Row { area.x } else { area.y };
                let mut prev: Option<(usize, f32)> = None;
                let lens = fit(children.iter().map(|c| if self.any_open(c) { c.size() / total } else { 0.0 }).collect(), room, if *axis == Axis::Row { MIN_W } else { MIN_H });
                for (i, c) in children.iter().enumerate() {
                    let open = self.any_open(c);
                    let len = lens[i];
                    if open && let Some((pi, pshare)) = prev {
                        let g = if *axis == Axis::Row { Rect { x: at - gap, y: area.y, w: gap, h: area.h } } else { Rect { x: area.x, y: at - gap, w: area.w, h: gap } };
                        gaps.push(Gap { trail: trail.clone(), index: pi, axis: *axis, rect: g, shares: (pshare, c.size()), room: room * (pshare + c.size()) / total });
                        let _ = i;
                    }
                    let r = if *axis == Axis::Row { Rect { x: at, y: area.y, w: len, h: area.h } } else { Rect { x: area.x, y: at, w: area.w, h: len } };
                    self.layout(c, r, out, gaps);
                    if open {
                        at += len + gap;
                        prev = Some((i, c.size()));
                    }
                }
            }
        }
    }

    fn any_open(&self, t: &PaneTree) -> bool {
        match t {
            PaneTree::Pane(p) => self.pane_open(p),
            PaneTree::Split { children, .. } => children.iter().any(|c| self.any_open(c)),
        }
    }

    /// move every pane toward where the tree now puts it. called while drawing.
    fn aim_panes(&mut self, tree: &PaneTree, area: Rect, settle: bool) {
        let mut targets = Vec::new();
        let mut gaps = Vec::new();
        self.layout(tree, area, &mut targets, &mut gaps);
        let mut leaves = Vec::new();
        tree.leaves(&mut leaves);
        let modules: HashMap<String, (Option<Module>, bool)> = leaves.iter().map(|p| (p.id.clone(), (p.module, self.pane_open(p)))).collect();
        // a maximized pane takes the whole room; the others fade where they are.
        let zoom = self.pane_zoom.clone().filter(|z| targets.iter().any(|(id, r)| id == z && r.w > 1.0));
        if zoom.is_some() {
            gaps.clear();
        }
        for (id, r) in &targets {
            let (module, open) = modules.get(id).copied().unwrap_or((None, true));
            let r = match &zoom {
                Some(z) if z == id => &area,
                _ => r,
            };
            let hidden = zoom.as_ref().is_some_and(|z| z != id);
            let alpha_to = if open && !hidden && r.w > 1.0 && r.h > 1.0 { 1.0 } else { 0.0 };
            match self.pane_fx.get_mut(id) {
                Some(fx) => {
                    fx.leaving = false;
                    fx.module = module;
                    for (spring, to) in fx.rect.iter_mut().zip([r.x, r.y, r.w, r.h]) {
                        if settle {
                            spring.snap(to);
                        } else if spring.target() != to {
                            spring.to(to, SMOOTH);
                        }
                    }
                    if settle {
                        fx.alpha.snap(alpha_to);
                    } else if fx.alpha.target() != alpha_to {
                        fx.alpha.to(alpha_to, SMOOTH);
                    }
                }
                None => {
                    // a new pane grows out of the middle of its place.
                    let inset = if settle { 0.0 } else { (r.w.min(r.h) * 0.06).min(24.0) };
                    let start = [r.x + inset, r.y + inset, (r.w - inset * 2.0).max(0.0), (r.h - inset * 2.0).max(0.0)];
                    let mut rect = start.map(Spring::points);
                    let mut alpha = Spring::unit(if settle { alpha_to } else { 0.0 });
                    if !settle {
                        for (spring, to) in rect.iter_mut().zip([r.x, r.y, r.w, r.h]) {
                            spring.to(to, SMOOTH);
                        }
                        alpha.to(alpha_to, SMOOTH);
                    }
                    self.pane_fx.insert(id.clone(), PaneFx { rect, alpha, module, leaving: false });
                }
            }
        }
        // panes no longer in the tree fade where they were.
        for (id, fx) in self.pane_fx.iter_mut() {
            if !modules.contains_key(id) && !fx.leaving {
                fx.leaving = true;
                fx.alpha.to(0.0, SMOOTH);
            }
        }
        self.pane_gaps = gaps.into_iter().map(|g| (g.trail, g.index, g.axis, g.rect, g.shares, g.room)).collect();
    }

    pub(crate) fn step_panes(&mut self, dt: f32, reduced: bool) -> bool {
        let mut still = false;
        for fx in self.pane_fx.values_mut() {
            for s in fx.rect.iter_mut() {
                still |= s.step(dt, reduced);
            }
            still |= fx.alpha.step(dt, reduced);
        }
        self.pane_fx.retain(|_, fx| !(fx.leaving && !fx.alpha.is_moving() && fx.alpha.value() <= 0.001));
        still
    }

    pub(crate) fn panes_moving(&self) -> bool {
        self.pane_fx.values().any(|fx| fx.alpha.is_moving() || fx.rect.iter().any(Spring::is_moving))
    }

    // ---- editing the tree ----------------------------------------------------------------

    /// change the layout of the first `panes` node in `ui.json`, writing the
    /// layout in use into the file first if it doesn't have one of its own.
    fn edit_panes(&mut self, change: impl FnOnce(&mut Value), window: &mut Window, cx: &mut Context<Self>) {
        let current = crate::ui::preset("default").and_then(|t| serde_json::from_str::<Value>(t).ok()).and_then(|v| v.get("root").cloned());
        self.edit_ui(
            |f| {
                if !f.contains_key("root") {
                    if let Some(root) = current {
                        f.insert("root".into(), root);
                    }
                    f.remove("preset");
                }
                if let Some(root) = f.get_mut("root")
                    && let Some(layout) = find_panes(root)
                {
                    change(layout);
                }
            },
            window,
            cx,
        );
    }

    pub(crate) fn close_pane(&mut self, trail: Vec<usize>, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_panes(|layout| remove_at(layout, &trail), window, cx);
    }

    /// split a pane in two. the new half gets the focus, as it does in a
    /// terminal: "split, then show X" puts X in the new pane, not over the old.
    pub(crate) fn split_pane(&mut self, trail: Vec<usize>, axis: Axis, window: &mut Window, cx: &mut Context<Self>) -> Option<String> {
        // named in the file, so it keeps its name whatever it goes on to show
        // (an unnamed pane is called after its module, and would be renamed).
        let taken: std::collections::HashSet<String> = self.ui.panes().into_iter().map(|p| p.id.clone()).collect();
        let id = (1..).map(|n| format!("pane-{n}")).find(|id| !taken.contains(id))?;
        self.edit_panes(|layout| split_at(layout, &trail, axis, &id), window, cx);
        self.ui.panes().iter().any(|p| p.id == id).then(|| {
            self.pane_focus = Some(id.clone());
            id
        })
    }

    pub(crate) fn set_pane_module(&mut self, trail: Vec<usize>, module: Module, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_panes(
            |layout| {
                if let Some(Value::Object(o)) = at_trail(layout, &trail) {
                    o.insert("module".into(), json!(module.name()));
                }
            },
            window,
            cx,
        );
        self.refresh_workspace();
    }

    fn start_pane_drag(&mut self, gap: usize, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some((trail, index, axis, _, shares, room)) = self.pane_gaps.get(gap).cloned() else { return };
        self.pane_drag = Some(PaneDrag { trail, index, axis, start: if axis == Axis::Row { x } else { y }, shares, room });
        cx.notify();
    }

    /// while dragging, the two neighbours trade room; the file is written when you let go.
    pub(crate) fn pane_drag_to(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some(d) = &self.pane_drag else { return };
        let moved = if d.axis == Axis::Row { x } else { y } - d.start;
        let total = d.shares.0 + d.shares.1;
        let per_point = total / d.room.max(1.0);
        // neither side gets smaller than a pane's least.
        let least = if d.axis == Axis::Row { MIN_W } else { MIN_H } * per_point;
        let a = (d.shares.0 + moved * per_point).clamp(least.min(total / 2.0), (total - least).max(total / 2.0));
        let (trail, index) = (d.trail.clone(), d.index);
        let mut ui = (*self.ui).clone();
        if let Some(PaneTree::Split { children, .. }) = ui.root_panes_mut().and_then(|t| t.at_mut(&trail)) {
            set_size(&mut children[index], a);
            set_size(&mut children[index + 1], total - a);
        }
        self.ui = Rc::new(ui);
        cx.notify();
    }

    pub(crate) fn pane_drag_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(d) = self.pane_drag.take() else { return };
        let sizes = match self.ui.root_panes().and_then(|t| t.at(&d.trail)) {
            Some(PaneTree::Split { children, .. }) => (children[d.index].size(), children[d.index + 1].size()),
            _ => return,
        };
        self.edit_panes(
            |layout| {
                if let Some(Value::Object(split)) = at_trail(layout, &d.trail)
                    && let Some(Value::Array(children)) = split.get_mut("children")
                {
                    for (i, size) in [(d.index, sizes.0), (d.index + 1, sizes.1)] {
                        if let Some(Value::Object(c)) = children.get_mut(i) {
                            c.insert("size".into(), json!((size * 100.0).round() / 100.0));
                        }
                    }
                }
            },
            window,
            cx,
        );
    }

    // ---- drawing -------------------------------------------------------------------------

    pub(crate) fn panes_el(&self, n: &Node, now: Millis, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let tree = n.props.layout.as_deref().cloned();
        let me = cx.entity();
        let area_cell = self.pane_area.clone();
        // measure the room the panes have, and lay them out in it on the next frame.
        let measure = canvas(
            move |bounds, window, cx| {
                let old = area_cell.replace(Some(bounds));
                if old != Some(bounds) {
                    // the room changed (the window was resized, a drawer moved):
                    // lay the panes out again on the very next frame, in front or not.
                    me.update(cx, |_, cx| cx.notify());
                    window.refresh();
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .inset_0();
        let mut layer = div().id(ElementId::from(SharedString::from(n.element_id()))).relative().size_full().child(measure);
        let Some(area) = self.pane_area.get() else { return layer.into_any_element() };
        let origin = area.origin;
        let Some(tree) = tree else { return layer.into_any_element() };
        let mut leaves = Vec::new();
        tree.leaves(&mut leaves);

        let mut ids: Vec<&String> = self.pane_fx.keys().collect();
        ids.sort();
        // a maximized pane is drawn last, over the others as they fade.
        if let Some(z) = &self.pane_zoom {
            ids.sort_by_key(|id| *id == z);
        }
        for id in ids {
            let fx = &self.pane_fx[id];
            let [x, y, w, h] = fx.rect.each_ref().map(Spring::value);
            let alpha = fx.alpha.value().clamp(0.0, 1.0);
            if alpha <= 0.001 || w < 1.0 || h < 1.0 {
                continue;
            }
            let pane = leaves.iter().find(|p| &p.id == id).copied();
            // the workspace starts near the window's left edge when nothing is beside it.
            let at_left = (x - f32::from(origin.x)).abs() < 1.0 && f32::from(origin.x) < theme::WINDOW_BUTTONS_W;
            let at_top = (y - f32::from(origin.y)).abs() < 1.0;
            layer = layer.child(
                div()
                    .absolute()
                    .left(px(x - f32::from(origin.x)))
                    .top(px(y - f32::from(origin.y)))
                    .w(px(w))
                    .h(px(h))
                    .opacity(alpha)
                    .child(self.pane_el(pane, fx.module, id, at_left && at_top, now, cx)),
            );
        }
        // the gaps can be grabbed to give one neighbour more room.
        let dragging = self.pane_drag.is_some();
        for (i, (_, _, axis, r, _, _)) in self.pane_gaps.iter().enumerate() {
            let axis = *axis;
            layer = layer.child(
                div()
                    .id(SharedString::from(format!("pane-gap-{i}")))
                    .absolute()
                    .left(px(r.x - f32::from(origin.x)))
                    .top(px(r.y - f32::from(origin.y)))
                    .w(px(r.w))
                    .h(px(r.h))
                    .when(axis == Axis::Row, |d| d.cursor_col_resize())
                    .when(axis == Axis::Column, |d| d.cursor_row_resize())
                    .when(dragging, |d| d.bg(p.quaternary.opacity(0.5)))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, cx| this.start_pane_drag(i, f32::from(ev.position.x), f32::from(ev.position.y), cx))),
            );
        }
        layer.children(self.drop_overlay(origin)).into_any_element()
    }

    /// keep the panes' targets in line with the tree and the room. called at the top of a frame.
    pub(crate) fn sync_panes(&mut self) {
        let Some(area) = self.pane_area.get() else { return };
        let Some(tree) = self.ui.root_panes().cloned() else {
            self.pane_fx.clear();
            return;
        };
        let rect = Rect { x: f32::from(area.origin.x), y: f32::from(area.origin.y), w: f32::from(area.size.width), h: f32::from(area.size.height) };
        // the first time there is room, everything just appears in place.
        let settle = self.pane_fx.is_empty() || !self.window_active;
        self.aim_panes(&tree, rect, settle);
    }

    fn pane_el(&self, pane: Option<&Pane>, module: Option<Module>, id: &str, under_window_buttons: bool, now: Millis, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let group = SharedString::from(format!("pane-{id}"));
        let trail = pane.map(|p| p.trail.clone()).unwrap_or_default();
        let live = pane.is_some();
        // the window's buttons sit over the top left pane when nothing else is there.
        let inset = if under_window_buttons { theme::WINDOW_BUTTONS_W * self.drawer_shut("sidebar") } else { 0.0 };

        let mut controls = h_flex().gap_0p5().flex_shrink_0();
        if live {
            controls = controls.child(self.pane_menu(id, &trail, module, cx));
        }
        // only worth marking when there is more than one pane to choose between.
        let open_panes = self.ui.panes().iter().filter(|p| self.pane_open(p)).count();
        let focused = self.pane_focus.as_deref() == Some(id) && open_panes > 1 && self.pane_zoom.is_none();
        let pid = id.to_owned();
        // the grip: hold it to carry the pane somewhere else.
        let grip = div()
            .id(SharedString::from(format!("pane-{id}-grip")))
            .flex_shrink_0()
            .cursor_grab()
            .text_color(p.tertiary)
            // shown on hover, so it isn't mistaken for the window's title bar.
            .invisible()
            .group_hover(group.clone(), |s| s.visible())
            .hover(|d| d.text_color(p.label))
            .tooltip(|window, cx| gpui_kit::component::tooltip::Tooltip::new("Drag onto another pane to move this one").build(window, cx))
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                this.pane_move = Some(PaneMove { id: pid.clone(), at: (f32::from(ev.position.x), f32::from(ev.position.y)), moved: false });
                cx.notify();
            }))
            .child(Icon::empty().path("icons/grip-vertical.svg").small());
        let header = h_flex()
            .id(SharedString::from(format!("pane-{id}-header")))
            .h(px(HEADER_H))
            .flex_shrink_0()
            .pl(px(6. + inset))
            .pr_2()
            .gap_2()
            .border_b_1()
            .border_color(p.separator)
            // a pane's header is title bar: it moves the window.
            .on_double_click(|_, window, _| window.titlebar_double_click())
            .on_mouse_move(self.moves_window(cx))
            .when(live, |d| d.child(grip))
            // the conversation has its own; any other pane in the corner gets one, so the sidebar is never out of reach.
            .when(under_window_buttons && module != Some(Module::Conversation) && !self.drawer("sidebar").is_some_and(|d| d.open), |d| {
                d.child(
                    Button::new(SharedString::from(format!("pane-{id}-sidebar")))
                        .ghost()
                        .xsmall()
                        .icon(Icon::empty().path("icons/panel-left.svg"))
                        .tooltip("Show Sidebar")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                )
            })
            .children(self.pane_title(module, id, now, cx))
            // the conversation and the workspace fill their own headers.
            .when(!matches!(module, Some(Module::Conversation | Module::Workspace)), |d| d.child(div().flex_1()))
            .child(controls)
            .test_support()
            .aria_label(SharedString::from(format!("{} pane", module.map_or("Empty", Module::title))));

        let body: AnyElement = match module {
            None => self.module_menu_grid(trail.clone(), cx).into_any_element(),
            Some(m) => self.module_el(m, pane, now, cx),
        };
        let fid = id.to_owned();
        let compact = self.ui.theme.compact.unwrap_or(false);
        v_flex()
            .group(group)
            .size_full()
            .when(!compact, |d| d.rounded(px(theme::R_PANEL)).border_1().shadow_sm())
            // the pane your keys act on wears a faint ring of the accent.
            .border_color(if focused { p.accent.opacity(0.35) } else { p.separator })
            .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, _| {
                if this.pane_focus.as_deref() != Some(fid.as_str()) {
                    this.pane_focus = Some(fid.clone());
                }
            }))
            .bg(p.window)
            .overflow_hidden()
            .child(header)
            .child(div().flex_1().min_h_0().overflow_hidden().child(body))
            .into_any_element()
    }

    /// what the header says: the module, or for the conversation the session itself.
    fn pane_title(&self, module: Option<Module>, pane_id: &str, now: Millis, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let p = self.palette.get();
        let icon = |name: &str| div().flex_shrink_0().child(Icon::empty().path(format!("icons/{name}.svg")).small().text_color(p.tertiary)).into_any_element();
        match module {
            Some(Module::Conversation) => {
                let ink = if self.window_active { p.label } else { p.secondary };
                let title = self.model.open_summary().filter(|_| !self.show_new).map_or_else(|| "New Session".to_owned(), |s| s.goal.clone());
                let mut out = vec![
                    Button::new("toolbar-sidebar")
                        .ghost()
                        .xsmall()
                        .icon(Icon::empty().path("icons/panel-left.svg"))
                        .tooltip(if self.drawer("sidebar").is_some_and(|d| d.open) { "Hide Sidebar" } else { "Show Sidebar" })
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx)))
                        .into_any_element(),
                    div().min_w_0().flex_shrink(1.0).truncate().set(Type::Headline).font_weight(FontWeight(550.)).text_color(ink).child(title).into_any_element(),
                ];
                if let Some(status) = self.status_line(now) {
                    out.push(div().min_w_0().flex_shrink(1.0).truncate().set(Type::Subheadline).text_color(p.tertiary).child(status).into_any_element());
                }
                // the header's own spacer comes next: these sit at its far end.
                out.push(div().flex_1().into_any_element());
                if self.model.open_summary().is_some_and(|s| s.run_state.is_busy()) && !self.show_new {
                    out.push(Button::new("toolbar-stop").xsmall().outline().label("Stop").tooltip("Stop the Agent").on_click(cx.listener(|this, _, _, cx| this.stop_agent(cx))).into_any_element());
                }
                out.push(self.add_menu("toolbar-new", cx).into_any_element());
                out.push(Button::new("toolbar-overview").ghost().xsmall().icon(Icon::empty().path("icons/layout-grid.svg")).tooltip("All Sessions").on_click(cx.listener(|this, _, _, cx| this.toggle_overview(cx))).into_any_element());
                if !self.show_new && self.model.open_summary().is_some() && self.has_pane("inspector") {
                    out.push(
                        Button::new("toolbar-inspector")
                            .ghost()
                            .xsmall()
                            .icon(Icon::empty().path("icons/panel-right.svg"))
                            .tooltip("Show or Hide the Workspace")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_inspector(cx)))
                            .into_any_element(),
                    );
                }
                out
            }
            // the workspace's header is its tabs.
            Some(Module::Workspace) => vec![div().id("pane-tabs").flex_1().min_w_0().overflow_x_scroll().child(self.panel_tabs_el(cx)).into_any_element()],
            // a terminal is called what its program says, else where it is.
            Some(Module::Terminal) => {
                let (title, exited) = self.term_title(pane_id).unwrap_or_else(|| ("Terminal".into(), None));
                vec![
                    icon(Module::Terminal.icon()),
                    div().min_w_0().flex_shrink(1.0).truncate().set(Type::Callout).font_weight(FontWeight(550.)).child(title).into_any_element(),
                    div().when(exited.is_none(), |d| d.hidden()).flex_shrink_0().child(theme::badge("exited", p.tertiary)).into_any_element(),
                ]
            }
            Some(m) => vec![icon(m.icon()), div().set(Type::Callout).font_weight(FontWeight(550.)).child(m.title()).into_any_element()],
            None => vec![icon("square-dashed"), div().set(Type::Callout).text_color(p.secondary).child("Empty Pane").into_any_element()],
        }
    }

    /// what a module looks like inside its pane.
    fn module_el(&self, m: Module, pane: Option<&Pane>, now: Millis, cx: &mut Context<Self>) -> AnyElement {
        let session = self.model.open_summary().filter(|_| !self.show_new);
        let quiet = |text: &str| div().p_4().set(Type::Callout).text_color(self.palette.get().secondary).child(text.to_owned()).into_any_element();
        match m {
            Module::Conversation => self.conversation_el(pane, cx),
            Module::Sessions => v_flex().size_full().child(self.sessions_list(now, cx)).into_any_element(),
            Module::Workspace => v_flex().size_full().child(self.panel_body(cx)).into_any_element(),
            // a terminal needs no session: without one it's a shell in your home folder.
            Module::Terminal => self.terminal_el(pane.map_or("", |p| p.id.as_str()), cx),
            _ if session.is_none() => quiet("Open a session to see this."),
            Module::Changes => div().size_full().child(self.changes_tab(cx)).into_any_element(),
            Module::Files => div().size_full().child(self.files_tab(cx)).into_any_element(),
            Module::Checks => div().size_full().child(self.checks_tab(cx)).into_any_element(),
            Module::Artifacts => div().size_full().child(self.artifacts_tab(session.expect("checked above"), cx)).into_any_element(),
            Module::Info => div().size_full().child(self.info_tab(session.expect("checked above"), cx)).into_any_element(),
            Module::Surfaces => self.surfaces_pane(cx),
            Module::Git => self.git_el(cx),
        }
    }

    /// the conversation module: the transcript, what is waiting on you, and the message box.
    fn conversation_el(&self, pane: Option<&Pane>, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        if self.show_new || self.model.open_summary().is_none() {
            return v_flex().size_full().children(self.error.clone().map(|e| div().px_4().py_2().set(Type::Callout).text_color(p.failed).child(e))).child(self.new_session_form(cx)).into_any_element();
        }
        let t = self.transcript_opts;
        let _ = pane;
        let column = || div().w_full().max_w(px(t.width)).px(px(t.gutter));
        // the message box: where most of the time goes, so it's the most
        // comfortable thing in the window. text at reading size, three lines to
        // start; controls at full size below; menus for / and @ above.
        let busy = self.model.open_summary().is_some_and(|s| s.run_state.is_busy());
        let send = if busy {
            theme::button(&p, "stop", "Stop", Some("square"), theme::ButtonKind::Secondary)
                .test_support()
                .aria_label("Stop")
                .on_click(cx.listener(|this, _, _, cx| this.stop_agent(cx)))
                .into_any_element()
        } else {
            theme::button(&p, "send", "", Some("arrow-up"), theme::ButtonKind::Primary)
                .w(theme::z(theme::CONTROL_H + 4.))
                .h(theme::z(theme::CONTROL_H + 4.))
                .px_0()
                .rounded_full()
                .test_support()
                .aria_label("Send")
                .on_click(cx.listener(|this, _, window, cx| this.send(window, cx)))
                .into_any_element()
        };
        let composer = v_flex()
            .id("composer-box")
            .relative()
            .px(theme::z(14.))
            .pt(theme::z(12.))
            .pb(theme::z(10.))
            .gap(theme::z(10.))
            .rounded(theme::z(theme::R_CARD + 4.))
            .border_1()
            .border_color(p.separator)
            .bg(p.raised)
            .shadow_md()
            // while a menu is up, its keys are its own.
            .children(self.mention_menu(cx))
            .children(self.attachment_chips(cx))
            .child(div().text_size(theme::z(14.)).line_height(theme::z(21.)).child(self.input_el(true, cx)))
            .child(
                h_flex()
                    .gap(theme::z(2.))
                    .items_center()
                    // in a narrow pane the pickers give way; send never does.
                    .child(h_flex().flex_1().min_w_0().overflow_hidden().gap(theme::z(2.)).child(self.plus_menu(cx)).child(self.model_chip(cx)).child(self.mode_menu(cx)))
                    .child(self.mcp_menu(cx))
                    .children(self.context_meter_el(cx))
                    .child(div().pl(theme::z(4.)).child(send)),
            );
        let hints = h_flex()
            .px(theme::z(12.))
            .pt(theme::z(6.))
            .gap(theme::z(14.))
            .set(Type::Subheadline)
            .text_color(p.tertiary)
            .child("⏎ send")
            .child("⇧⏎ new line")
            .child("/ commands")
            .child("@ files");
        let composer = v_flex().child(composer).child(hints);
        v_flex()
            .size_full()
            .children(self.error.clone().map(|e| div().px_4().py_2().set(Type::Callout).text_color(p.failed).child(e)))
            .child(div().flex_1().min_h_0().relative().child(self.transcript_el(cx).size_full()).child(self.message_rail(cx)))
            .child(h_flex().justify_center().children(self.prompts_el(cx).map(|d| column().child(d))))
            .child(h_flex().justify_center().pb_4().pt_1().child(column().child(composer)))
            .into_any_element()
    }

    /// an empty pane offers every module.
    fn module_menu_grid(&self, trail: Vec<usize>, cx: &mut Context<Self>) -> impl IntoElement {
        let p = self.palette.get();
        let taken: Vec<Module> = self.ui.panes().iter().filter_map(|p| p.module).collect();
        // a list, not a grid: it reads the same in a narrow pane as in a wide one.
        let mut list = theme::float_list(&p).w_full().max_w(px(360.)).rounded(px(theme::R_PANEL)).border_1().border_color(p.separator).child(theme::float_label(&p, "Show in this pane"));
        for (_, m, title, icon, about) in MODULES {
            // one conversation, one session list, one workspace.
            let single = matches!(m, Module::Conversation | Module::Sessions | Module::Workspace);
            if single && taken.contains(&m) {
                continue;
            }
            let t = trail.clone();
            list = list.child(
                theme::float_item(&p, SharedString::from(format!("add-{}", m.name())), Some(icon), title, Some(about), false)
                    .test_support()
                    .aria_label(SharedString::from(format!("Show {title} here")))
                    .on_click(cx.listener(move |this, _, window, cx| this.set_pane_module(t.clone(), m, window, cx))),
            );
        }
        // if this is the only pane left, the way back is right here.
        let alone = self.ui.panes().len() == 1;
        v_flex()
            .id("module-list")
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_3()
            .overflow_y_scroll()
            .child(list)
            .when(alone, |d| {
                d.child(Button::new("empty-reset-layout").ghost().small().label("Reset to the default layout").on_click(cx.listener(|this, _, window, cx| {
                    this.edit_ui(
                        |f| {
                            f.remove("root");
                            f.remove("preset");
                        },
                        window,
                        cx,
                    )
                })))
            })
    }

    /// a pane's menu: split, maximize, what it shows, close. every one has a key.
    fn pane_menu(&self, id: &str, trail: &[usize], current: Option<Module>, cx: &mut Context<Self>) -> impl IntoElement {
        let me = cx.entity().downgrade();
        let (trail, pid) = (trail.to_vec(), id.to_owned());
        let p = self.palette.get();
        Popover::new(SharedString::from(format!("pane-{id}-menu")))
            .p_0()
            .anchor(Anchor::TopRight)
            .trigger(Button::new(SharedString::from(format!("pane-{id}-more"))).ghost().xsmall().icon(Icon::empty().path("icons/ellipsis.svg").text_color(p.tertiary)).tooltip("Pane"))
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let zoomed = this.pane_zoom.as_deref() == Some(pid.as_str());
                let taken: Vec<Module> = this.ui.panes().iter().filter_map(|p| p.module).collect();
                let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| f(this, window, cx));
                    }
                };
                let (t1, t2, t3, z) = (trail.clone(), trail.clone(), trail.clone(), pid.clone());
                let sep = || theme::float_sep(&p);
                let mut list = theme::float_list(&p)
                    .w(px(248.))
                    .child(theme::float_item_with(&p, SharedString::from(format!("pane-{pid}-split-right")), Some("columns-2"), "Split right", None, Some("⌘ D"), false).on_click(act(Box::new(move |this, w, cx| { this.split_pane(t1.clone(), Axis::Row, w, cx); }))).test_support())
                    .child(theme::float_item_with(&p, SharedString::from(format!("pane-{pid}-split-down")), Some("rows-2"), "Split down", None, Some("⇧ ⌘ D"), false).on_click(act(Box::new(move |this, w, cx| { this.split_pane(t2.clone(), Axis::Column, w, cx); }))).test_support())
                    .child(
                        theme::float_item_with(&p, SharedString::from(format!("pane-{pid}-zoom")), Some(if zoomed { "minimize-2" } else { "maximize-2" }), if zoomed { "Restore" } else { "Maximize" }, None, Some("⇧ ⌘ ↩"), false)
                            .on_click(act(Box::new(move |this, _, cx| this.zoom_pane(&z, cx))))
                            .test_support(),
                    )
                    .child(sep())
                    .child(theme::float_label(&p, "Show here"));
                for (_, m, title, icon, _) in MODULES {
                    let single = matches!(m, Module::Conversation | Module::Sessions | Module::Workspace);
                    if single && taken.contains(&m) && current != Some(m) {
                        continue;
                    }
                    let t = trail.clone();
                    list = list.child(theme::float_item(&p, SharedString::from(format!("pane-{pid}-show-{}", m.name())), Some(icon), title, None, current == Some(m)).on_click(act(Box::new(move |this, w, cx| this.set_pane_module(t.clone(), m, w, cx)))).test_support());
                }
                list.child(sep())
                    .child(theme::float_item_with(&p, SharedString::from(format!("pane-{pid}-close")), Some("x"), "Close pane", None, Some("⌘ W"), false).on_click(act(Box::new(move |this, w, cx| this.close_pane(t3.clone(), w, cx)))).test_support())
                    .into_any_element()
            })
    }

    /// maximize a pane over the others, or put it back.
    pub(crate) fn zoom_pane(&mut self, id: &str, cx: &mut Context<Self>) {
        self.pane_zoom = if self.pane_zoom.as_deref() == Some(id) { None } else { Some(id.to_owned()) };
        cx.notify();
    }

    /// where a pane is heading on screen, in points: x, y, width, height.
    pub(crate) fn pane_rect(&self, id: &str) -> Option<[f32; 4]> {
        self.pane_fx.get(id).filter(|fx| !fx.leaving).map(|fx| fx.rect.each_ref().map(|s| s.target().round()))
    }

    /// the pane keys act on: the one last clicked in, or the conversation.
    pub(crate) fn focused_pane(&self) -> Option<Pane> {
        let panes = self.ui.panes();
        self.pane_focus
            .as_deref()
            .and_then(|f| panes.iter().find(|p| p.id == f))
            .or_else(|| panes.iter().find(|p| p.module == Some(Module::Conversation)))
            .or_else(|| panes.first())
            .map(|p| (*p).clone())
    }

    pub fn split_focused_right(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.focused_pane() {
            self.split_pane(p.trail, Axis::Row, window, cx);
        }
    }

    pub fn split_focused_down(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.focused_pane() {
            self.split_pane(p.trail, Axis::Column, window, cx);
        }
    }

    pub fn close_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(p) = self.focused_pane() {
            self.pane_focus = None;
            self.close_pane(p.trail, window, cx);
        }
    }

    pub fn zoom_focused(&mut self, cx: &mut Context<Self>) {
        if let Some(p) = self.focused_pane() {
            self.zoom_pane(&p.id, cx);
        }
    }

    // ---- carrying a pane ---------------------------------------------------------------

    /// while a pane is carried: which pane it is over, and where it would land.
    fn drop_target(&self, x: f32, y: f32) -> Option<(String, Zone)> {
        let mv = self.pane_move.as_ref()?;
        for (id, fx) in &self.pane_fx {
            if id == &mv.id || fx.leaving {
                continue;
            }
            let [rx, ry, rw, rh] = fx.rect.each_ref().map(Spring::target);
            if x < rx || x > rx + rw || y < ry || y > ry + rh {
                continue;
            }
            // the outer quarter of each side docks on that side; the middle swaps.
            let (fx_, fy) = ((x - rx) / rw.max(1.0), (y - ry) / rh.max(1.0));
            let zone = match (fx_, fy) {
                (f, _) if f < 0.25 => Zone::Left,
                (f, _) if f > 0.75 => Zone::Right,
                (_, f) if f < 0.25 => Zone::Top,
                (_, f) if f > 0.75 => Zone::Bottom,
                _ => Zone::Swap,
            };
            return Some((id.clone(), zone));
        }
        None
    }

    pub(crate) fn pane_move_to(&mut self, x: f32, y: f32, cx: &mut Context<Self>) {
        let Some(mv) = self.pane_move.as_mut() else { return };
        if (x - mv.at.0).abs() + (y - mv.at.1).abs() > 4.0 {
            mv.moved = true;
        }
        mv.at = (x, y);
        cx.notify();
    }

    pub(crate) fn pane_move_end(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(mv) = self.pane_move.as_ref() else { return };
        let target = if mv.moved { self.drop_target(mv.at.0, mv.at.1) } else { None };
        let from = mv.id.clone();
        self.pane_move = None;
        let Some((to, zone)) = target else {
            cx.notify();
            return;
        };
        self.move_pane_to(from, to, zone, window, cx);
    }

    /// carry pane `from` to pane `to`: beside it on a side, or into its place.
    pub(crate) fn move_pane_to(&mut self, from: String, to: String, zone: Zone, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<(Vec<usize>, String)> = self.ui.panes().iter().map(|p| (p.trail.clone(), p.id.clone())).collect();
        self.edit_panes(
            move |layout| {
                // every pane is named in the file first, so it can be found after the tree changes.
                for (trail, id) in &ids {
                    if let Some(Value::Object(o)) = at_trail(layout, trail) {
                        o.entry("id").or_insert(json!(id));
                    }
                }
                move_pane(layout, &from, &to, zone);
            },
            window,
            cx,
        );
    }

    /// what a carried pane shows over the pane it would land on.
    fn drop_overlay(&self, origin: Point<Pixels>) -> Option<AnyElement> {
        let mv = self.pane_move.as_ref().filter(|m| m.moved)?;
        let p = self.palette.get();
        let (id, zone) = self.drop_target(mv.at.0, mv.at.1)?;
        let [x, y, w, h] = self.pane_fx.get(&id)?.rect.each_ref().map(Spring::target);
        let (x, y, w, h) = match zone {
            Zone::Left => (x, y, w / 2.0, h),
            Zone::Right => (x + w / 2.0, y, w / 2.0, h),
            Zone::Top => (x, y, w, h / 2.0),
            Zone::Bottom => (x, y + h / 2.0, w, h / 2.0),
            Zone::Swap => (x, y, w, h),
        };
        let o = (f32::from(origin.x), f32::from(origin.y));
        Some(
            div()
                .absolute()
                .left(px(x - o.0 + 4.0))
                .top(px(y - o.1 + 4.0))
                .w(px((w - 8.0).max(0.0)))
                .h(px((h - 8.0).max(0.0)))
                .rounded(px(theme::R_PANEL - 4.0))
                .border_2()
                .border_color(p.accent.opacity(0.6))
                .bg(p.accent.opacity(0.08))
                .flex()
                .items_center()
                .justify_center()
                .child(div().px_3().py_1().rounded_full().bg(p.raised).shadow_sm().set(Type::Callout).text_color(p.accent).child(match zone {
                    Zone::Swap => "Swap",
                    Zone::Left => "Dock left",
                    Zone::Right => "Dock right",
                    Zone::Top => "Dock above",
                    Zone::Bottom => "Dock below",
                }))
                .into_any_element(),
        )
    }
}

/// where a carried pane lands on another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Zone {
    Left,
    Right,
    Top,
    Bottom,
    /// trade places.
    Swap,
}

/// a pane being carried by its grip.
pub(crate) struct PaneMove {
    id: String,
    at: (f32, f32),
    /// far enough from where it was picked up to be a drag, not a click.
    moved: bool,
}

// ---- the tree, as json -----------------------------------------------------------------

impl crate::ui::Ui {
    /// the layout of the first `panes` node.
    pub(crate) fn root_panes(&self) -> Option<&PaneTree> {
        self.nodes().into_iter().find_map(|n| n.props.layout.as_deref())
    }

    pub(crate) fn root_panes_mut(&mut self) -> Option<&mut PaneTree> {
        let mut found: Option<*mut PaneTree> = None;
        self.root.walk_mut(&mut |n| {
            if found.is_none()
                && let Some(t) = n.props.layout.as_deref_mut()
            {
                found = Some(t as *mut PaneTree);
            }
        });
        // SAFETY: the pointer is into `self.root`, which is borrowed mutably for
        // as long as the returned reference lives, and nothing else touches it.
        found.map(|t| unsafe { &mut *t })
    }
}

impl PaneTree {
    pub(crate) fn at(&self, trail: &[usize]) -> Option<&PaneTree> {
        match (trail.split_first(), self) {
            (None, t) => Some(t),
            (Some((i, rest)), PaneTree::Split { children, .. }) => children.get(*i)?.at(rest),
            _ => None,
        }
    }

    pub(crate) fn at_mut(&mut self, trail: &[usize]) -> Option<&mut PaneTree> {
        match (trail.split_first(), self) {
            (None, t) => Some(t),
            (Some((i, rest)), PaneTree::Split { children, .. }) => children.get_mut(*i)?.at_mut(rest),
            _ => None,
        }
    }
}

fn set_size(t: &mut PaneTree, size: f32) {
    match t {
        PaneTree::Pane(p) => p.size = size,
        PaneTree::Split { size: s, .. } => *s = size,
    }
}

/// the `layout` of the first `panes` node in a json tree.
fn find_panes(v: &mut Value) -> Option<&mut Value> {
    let is_panes = v.get("type").and_then(Value::as_str) == Some("panes");
    if is_panes {
        return v.get_mut("layout");
    }
    v.get_mut("children")?.as_array_mut()?.iter_mut().find_map(find_panes)
}

fn at_trail<'a>(v: &'a mut Value, trail: &[usize]) -> Option<&'a mut Value> {
    match trail.split_first() {
        None => Some(v),
        Some((i, rest)) => at_trail(v.get_mut("children")?.get_mut(*i)?, rest),
    }
}

/// take a pane out. a split left with one child becomes that child.
fn remove_at(layout: &mut Value, trail: &[usize]) {
    let Some((last, parent)) = trail.split_last() else {
        // the only pane: it becomes an empty one, offering modules.
        *layout = json!({});
        return;
    };
    let Some(split) = at_trail(layout, parent) else { return };
    let size = split.get("size").cloned();
    if let Some(Value::Array(children)) = split.get_mut("children") {
        if *last < children.len() {
            children.remove(*last);
        }
        if children.len() == 1 {
            let mut only = children.remove(0);
            if let (Some(size), Value::Object(o)) = (size, &mut only) {
                o.insert("size".into(), size);
            } else if let Value::Object(o) = &mut only {
                o.remove("size");
            }
            *split = only;
        }
    }
}

/// put an empty pane beside one: into its split if that runs the same way, or a new split.
fn split_at(layout: &mut Value, trail: &[usize], axis: Axis, id: &str) {
    let name = if axis == Axis::Row { "row" } else { "column" };
    if let Some((last, parent)) = trail.split_last()
        && let Some(split) = at_trail(layout, parent)
        && split.get("split").and_then(Value::as_str) == Some(name)
        && let Some(Value::Array(children)) = split.get_mut("children")
    {
        let size = children.get(*last).and_then(|c| c.get("size")).and_then(Value::as_f64).unwrap_or(1.0);
        children.insert(last + 1, json!({ "id": id, "size": size }));
        return;
    }
    let Some(target) = at_trail(layout, trail) else { return };
    let mut leaf = target.take();
    let size = leaf.get("size").cloned();
    if let Value::Object(o) = &mut leaf {
        o.remove("size");
    }
    let mut split = json!({ "split": name, "children": [leaf, { "id": id }] });
    if let Some(size) = size {
        split["size"] = size;
    }
    *target = split;
}

/// where in the layout the pane with this id is.
fn trail_of(v: &Value, id: &str) -> Option<Vec<usize>> {
    if v.get("id").and_then(Value::as_str) == Some(id) {
        return Some(Vec::new());
    }
    for (i, c) in v.get("children")?.as_array()?.iter().enumerate() {
        if let Some(mut t) = trail_of(c, id) {
            t.insert(0, i);
            return Some(t);
        }
    }
    None
}

/// carry pane `from` to pane `to`: beside it on one side, or into its place.
fn move_pane(layout: &mut Value, from: &str, to: &str, zone: Zone) {
    let (Some(a), Some(b)) = (trail_of(layout, from), trail_of(layout, to)) else { return };
    if zone == Zone::Swap {
        let va = at_trail(layout, &a).map(Value::take);
        let vb = at_trail(layout, &b).map(Value::take);
        if let (Some(mut va), Some(mut vb)) = (va, vb) {
            // each keeps the size of the place it moves into.
            let (sa, sb) = (va.get("size").cloned(), vb.get("size").cloned());
            for (v, size) in [(&mut va, sb), (&mut vb, sa)] {
                if let Value::Object(o) = v {
                    match size {
                        Some(s) => o.insert("size".into(), s),
                        None => o.remove("size"),
                    };
                }
            }
            if let Some(slot) = at_trail(layout, &a) {
                *slot = vb;
            }
            if let Some(slot) = at_trail(layout, &b) {
                *slot = va;
            }
        }
        return;
    }
    let Some(mut moving) = at_trail(layout, &a).cloned() else { return };
    if let Value::Object(o) = &mut moving {
        o.remove("size");
    }
    remove_at(layout, &a);
    let Some(b) = trail_of(layout, to) else { return };
    let (axis, before) = match zone {
        Zone::Left => ("row", true),
        Zone::Right => ("row", false),
        Zone::Top => ("column", true),
        _ => ("column", false),
    };
    // into the split it is in, if that runs the same way; otherwise a new split around it.
    if let Some((last, parent)) = b.split_last()
        && let Some(split) = at_trail(layout, parent)
        && split.get("split").and_then(Value::as_str) == Some(axis)
        && let Some(Value::Array(children)) = split.get_mut("children")
    {
        let size = children.get(*last).and_then(|c| c.get("size")).cloned().unwrap_or(json!(1));
        if let Value::Object(o) = &mut moving {
            o.insert("size".into(), size);
        }
        children.insert(if before { *last } else { last + 1 }, moving);
        return;
    }
    let Some(target) = at_trail(layout, &b) else { return };
    let mut there = target.take();
    let size = there.get("size").cloned();
    if let Value::Object(o) = &mut there {
        o.remove("size");
    }
    let children = if before { vec![moving, there] } else { vec![there, moving] };
    let mut split = json!({ "split": axis, "children": children });
    if let Some(size) = size {
        split["size"] = size;
    }
    *target = split;
}

#[cfg(test)]
mod tests {
    // named imports: a wildcard would also bring in the ui library's own `test` attribute.
    use super::{Axis, Zone, fit, move_pane, remove_at, split_at};
    use serde_json::json;

    #[test]
    fn a_pane_keeps_its_least_room_while_there_is_room_for_it() {
        let near = |a: Vec<f32>, b: [f32; 2]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 0.01);
        assert!(near(fit(vec![0.6, 0.4], 600.0, 280.0), [320.0, 280.0]), "the small one is held at its least, the other gives");
        assert!(near(fit(vec![0.6, 0.4], 1000.0, 280.0), [600.0, 400.0]), "with room, shares as asked");
        assert!(near(fit(vec![0.6, 0.4], 400.0, 280.0), [400.0, 0.0]), "without room for both, the second folds away");
        assert!(near(fit(vec![1.0, 0.0], 500.0, 280.0), [500.0, 0.0]), "a folded pane stays folded");
    }

    #[test]
    fn closing_a_pane_folds_its_split_and_splitting_adds_a_sibling() {
        let mut l = json!({"split": "row", "children": [{"module": "conversation", "size": 3}, {"split": "column", "size": 2, "children": [{"module": "changes"}, {"module": "files"}]}]});
        remove_at(&mut l, &[1, 1]);
        assert_eq!(l, json!({"split": "row", "children": [{"module": "conversation", "size": 3}, {"module": "changes", "size": 2}]}), "a split of one is just that one, in its place");
        split_at(&mut l, &[0], Axis::Row, "pane-1");
        assert_eq!(l["children"].as_array().unwrap().len(), 3, "a row split again to the right gets a sibling, not a nested split");
        assert_eq!(l["children"][1], json!({"id": "pane-1", "size": 3.0}), "named, and the size of what it split");
        split_at(&mut l, &[2], Axis::Column, "pane-2");
        assert_eq!(l["children"][2], json!({"split": "column", "size": 2, "children": [{"module": "changes"}, {"id": "pane-2"}]}));
        // carry changes to the right of files, then swap it with the conversation.
        let mut l = json!({"split": "row", "children": [{"id": "c", "module": "conversation", "size": 3}, {"id": "w", "split": "column", "size": 2, "children": [{"id": "ch", "module": "changes"}, {"id": "f", "module": "files"}]}]});
        move_pane(&mut l, "ch", "f", Zone::Right);
        // files is left alone in its column, so the column folds into it; changes lands beside it in the row.
        assert_eq!(l["children"].as_array().unwrap().len(), 3, "{l}");
        assert_eq!((l["children"][1]["id"].as_str(), l["children"][2]["id"].as_str()), (Some("f"), Some("ch")));
        assert_eq!(l["children"][2]["size"], 2, "it takes the size of what it docks beside");
        move_pane(&mut l, "ch", "c", Zone::Swap);
        assert_eq!(l["children"][0]["id"], "ch");
        assert_eq!(l["children"][0]["size"], 3, "it takes the size of the place it moves into");
        move_pane(&mut l, "f", "ch", Zone::Top);
        assert_eq!(l["children"][0]["split"], "column", "{l}");
        assert_eq!(l["children"][0]["children"][0]["id"], "f");
        let mut one = json!({"module": "conversation"});
        remove_at(&mut one, &[]);
        assert_eq!(one, json!({}), "the last pane closes to an empty one");
    }
}
