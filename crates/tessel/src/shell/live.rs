use super::*;
use gpui_kit::component::Icon;
use tessel_proto::item::{ItemBody, ToolStatus};
use tessel_proto::task::{Task, TaskState};

pub(crate) const MOST_FOLLOWED: usize = 8;

pub(crate) struct Step {
    pub icon: &'static str,
    pub verb: String,
    pub what: String,
    pub code: bool,
    pub running: bool,
    pub failed: bool,
}

fn busy(s: TaskState) -> bool {
    matches!(s, TaskState::Running | TaskState::Proving | TaskState::Landing | TaskState::Checking)
}

impl Shell {
    pub(crate) fn factory_follows(&self) -> Vec<SessionId> {
        if !self.factory_on() {
            return Vec::new();
        }
        let mut live: Vec<&Task> = self.tasks.iter().filter(|t| busy(t.state) && t.session.is_some()).collect();
        live.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        let mut out: Vec<SessionId> = live.into_iter().filter_map(|t| t.session).take(MOST_FOLLOWED).collect();
        if let Some(s) = self.stage_pick.as_ref().and_then(|id| self.tasks.iter().find(|t| &t.id == id)).and_then(|t| t.session)
            && !out.contains(&s)
        {
            out.push(s);
        }
        out
    }

    fn items_of(&self, session: SessionId) -> Option<&[tessel_proto::Item]> {
        if let Some(o) = self.model.open.as_ref().filter(|o| o.id == session) {
            return Some(o.items());
        }
        self.model.others.get(&session).map(|o| o.items())
    }

    pub(crate) fn steps(&self, session: SessionId, most: usize) -> Vec<Step> {
        let Some(items) = self.items_of(session) else { return Vec::new() };
        let mut out: Vec<Step> = Vec::new();
        for it in items.iter().rev() {
            if out.len() >= most {
                break;
            }
            match &it.body {
                ItemBody::Tool { tool, input, status, .. } => {
                    if tool == "TodoWrite" {
                        continue;
                    }
                    let (icon, verb, what, code) = super::tool_parts(tool, input);
                    out.push(Step { icon, verb, what, code, running: matches!(status, ToolStatus::Running | ToolStatus::Pending), failed: matches!(status, ToolStatus::Failed) });
                }
                ItemBody::Text { text, .. } if it.parent.is_none() => {
                    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").trim_start_matches('#').trim().to_owned();
                    if !line.is_empty() {
                        out.push(Step { icon: "message-square", verb: String::new(), what: line, code: false, running: false, failed: false });
                    }
                }
                ItemBody::User { .. } => break,
                _ => {}
            }
        }
        out.reverse();
        out
    }

    pub(crate) fn feed_el(&self, session: SessionId, most: usize) -> Option<Div> {
        let p = self.palette.get();
        let steps = self.steps(session, most);
        if steps.is_empty() {
            return None;
        }
        let last = steps.len().saturating_sub(1);
        let mut col = v_flex().w_full().gap(theme::z(2.));
        for (i, s) in steps.into_iter().enumerate() {
            let now = i == last;
            col = col.child(
                h_flex()
                    .w_full()
                    .h(theme::z(26.))
                    .gap(theme::z(10.))
                    .items_center()
                    .child(Icon::empty().path(format!("icons/{}.svg", s.icon)).size(theme::z(13.)).text_color(if s.failed { p.failed } else if now { p.label } else { p.tertiary }))
                    .when(!s.verb.is_empty(), |d| d.child(div().flex_shrink_0().set(Type::Subheadline).font_weight(FontWeight::MEDIUM).text_color(if now { p.label } else { p.secondary }).child(s.verb)))
                    .child(div().flex_1().min_w_0().truncate().set(if s.code { Type::Code } else { Type::Subheadline }).text_color(if now { p.label } else { p.secondary }).child(s.what))
                    .when(s.running, |d| d.child(super::live_dot(p.info, &p))),
            );
        }
        Some(col)
    }

    pub(crate) fn all_work(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let mine = |t: &&Task| self.board_project.as_ref().is_none_or(|r| &t.repo == r);
        let mut live: Vec<&Task> = self.tasks.iter().filter(|t| busy(t.state) || t.state == TaskState::Planned).filter(mine).collect();
        live.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        let needs: Vec<&Task> = self.tasks.iter().filter(|t| t.state == TaskState::Review && !t.cleared).filter(mine).collect();
        let mut col = v_flex().w_full().max_w(theme::z(1180.)).mx_auto().px(theme::z(40.)).pt(theme::z(28.)).pb(theme::z(48.)).gap(theme::z(20.));
        let head = format!("{} working", live.len()) + &if needs.is_empty() { String::new() } else { format!(", {} need{} you", needs.len(), if needs.len() == 1 { "s" } else { "" }) };
        col = col.child(div().font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(28.)).text_color(p.label).child(head));
        if !needs.is_empty() {
            let mut row = h_flex().w_full().flex_wrap().gap(theme::z(8.));
            for t in &needs {
                let id = t.id.clone();
                row = row.child(
                    h_flex()
                        .id(SharedString::from(format!("all-needs-{}", t.id)))
                        .h(theme::z(32.))
                        .px(theme::z(12.))
                        .gap(theme::z(8.))
                        .items_center()
                        .rounded(theme::z(theme::by_height(32.)))
                        .bg(p.fill)
                        .cursor_pointer()
                        .hover(|d| d.bg(p.hover))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stage_pick = Some(id.clone());
                            this.stage_shot = 0;
                            this.stage_swatch = None;
                            cx.notify();
                        }))
                        .test_support()
                        .child(div().size(theme::z(7.)).rounded_full().bg(p.blocked))
                        .child(div().max_w(theme::z(280.)).truncate().set(Type::Subheadline).text_color(p.label).child(t.title.clone())),
                );
            }
            col = col.child(row);
        }
        if live.is_empty() {
            return col.child(div().pt(theme::z(12.)).set(Type::Body).text_color(p.secondary).child("Nothing is running.")).into_any_element();
        }
        let card_h = 300.;
        let mut grid = h_flex().w_full().flex_wrap().gap(theme::z(14.));
        for t in live {
            let id = t.id.clone();
            let doing = self.stage_doing(t);
            let feed = t.session.and_then(|s| self.feed_el(s, 8));
            grid = grid.child(
                v_flex()
                    .id(SharedString::from(format!("all-task-{}", t.id)))
                    .flex_basis(relative(0.48))
                    .flex_grow(1.)
                    .min_w(theme::z(340.))
                    .h(theme::z(card_h))
                    .p(theme::z(18.))
                    .gap(theme::z(12.))
                    .rounded(theme::z(theme::outer(theme::by_height(26.), 18.)))
                    .bg(p.raised)
                    .border_1()
                    .border_color(p.separator)
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(|d| d.border_color(p.tertiary))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.stage_pick = Some(id.clone());
                        this.stage_shot = 0;
                        this.stage_swatch = None;
                        cx.notify();
                    }))
                    .test_support()
                    .child(
                        v_flex()
                            .gap(theme::z(4.))
                            .child(div().truncate().font_weight(FontWeight::SEMIBOLD).set(Type::Body).text_color(p.label).child(t.title.clone()))
                            .child(h_flex().gap(theme::z(8.)).items_center().child(div().w(theme::z(12.)).flex().justify_center().child(super::status_dot(p.info, true, &p))).child(div().truncate().set(Type::Subheadline).text_color(p.secondary).child(doing))),
                    )
                    .child(div().h(px(1.)).w_full().bg(p.separator))
                    .child(match feed {
                        Some(f) => div().flex_1().min_h_0().flex().flex_col().justify_end().child(f).into_any_element(),
                        None => div().set(Type::Subheadline).text_color(p.tertiary).child("Starting…").into_any_element(),
                    }),
            );
        }
        col.child(grid).into_any_element()
    }
}
