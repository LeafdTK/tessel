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
        let now = super::now_ms();
        let mine = |t: &&Task| self.board_project.as_ref().is_none_or(|r| &t.repo == r) && !t.cleared;
        let sorted = |states: &[TaskState]| {
            let mut v: Vec<&Task> = self.tasks.iter().filter(|t| states.contains(&t.state)).filter(mine).collect();
            v.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
            v
        };
        let needs = sorted(&[TaskState::Review]);
        let live = sorted(&[TaskState::Running, TaskState::Proving, TaskState::Landing, TaskState::Checking, TaskState::Planned]);
        let next = sorted(&[TaskState::Ready]);
        let landed: Vec<&Task> = sorted(&[TaskState::Landed]).into_iter().take(20).collect();
        let mut col = v_flex().w_full().min_w_0().max_w(theme::z(1080.)).mx_auto().px(theme::z(40.)).pt(theme::z(28.)).pb(theme::z(48.)).gap(theme::z(8.));
        let mut counts = Vec::new();
        for (n, word) in [(needs.len(), "need you"), (live.len(), "working"), (next.len(), "next"), (landed.len(), "landed")] {
            if n > 0 {
                counts.push(format!("{n} {word}"));
            }
        }
        col = col.child(div().pb(theme::z(8.)).font_family(theme::DEFAULT_FONT).font_weight(FontWeight::BOLD).text_size(theme::z(28.)).text_color(p.label).child(if counts.is_empty() { "Nothing yet".to_owned() } else { counts.join(" · ") }));
        let heading = |text: &str, n: usize| {
            h_flex()
                .pt(theme::z(18.))
                .pb(theme::z(6.))
                .px(theme::z(14.))
                .gap(theme::z(8.))
                .items_baseline()
                .child(div().set(Type::Subheadline).font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(text.to_owned()))
                .child(div().set(Type::Subheadline).font_features(theme::tabular()).text_color(p.tertiary).child(n.to_string()))
        };
        if !needs.is_empty() {
            col = col.child(heading("Needs you", needs.len()));
            for t in &needs {
                let line = t.ask.clone().unwrap_or_else(|| self.outcome(t));
                col = col.child(self.inbox_row(t, div().size(theme::z(8.)).rounded_full().bg(p.blocked).into_any_element(), line, p.blocked, now, cx));
            }
        }
        if !live.is_empty() {
            col = col.child(heading("Working", live.len()));
            let mut grid = h_flex().w_full().flex_wrap().gap(theme::z(12.));
            for t in &live {
                grid = grid.child(self.work_card(t, cx));
            }
            col = col.child(grid);
        }
        if !next.is_empty() {
            col = col.child(heading("Next", next.len()));
            for t in &next {
                let line = if t.deps.is_empty() { "Queued, starts when a slot is free".to_owned() } else { format!("After {}", t.deps.iter().filter_map(|d| self.tasks.iter().find(|x| &x.id == d)).map(|x| x.title.clone()).collect::<Vec<_>>().join(", ")) };
                col = col.child(self.inbox_row(t, div().size(theme::z(8.)).rounded_full().border_1().border_color(p.tertiary).into_any_element(), line, p.secondary, now, cx));
            }
        }
        if !landed.is_empty() {
            col = col.child(heading("Landed", landed.len()));
            for t in &landed {
                let line = t.review.clone().unwrap_or_else(|| self.outcome(t));
                col = col.child(self.inbox_row(t, Icon::empty().path("icons/check.svg").size(theme::z(13.)).text_color(p.finished).into_any_element(), line, p.secondary, now, cx));
            }
        }
        col.into_any_element()
    }

    fn inbox_row(&self, t: &Task, mark: AnyElement, line: String, tone: Hsla, now: u64, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let id = t.id.clone();
        let (added, removed) = t.changes.iter().fold((0u32, 0u32), |(a, r), f| (a + f.added, r + f.removed));
        let link: Option<(String, String)> = match (&t.pr, &t.link, &t.landed) {
            (Some(pr), _, _) => Some((pr.rsplit('/').next().filter(|n| n.chars().all(|c| c.is_ascii_digit())).map_or_else(|| "Pull request".to_owned(), |n| format!("PR #{n}")), pr.clone())),
            (None, Some(url), Some(l)) if !l.contains('/') => Some((l.chars().take(7).collect(), url.clone())),
            (None, Some(url), Some(l)) => Some((l.clone(), url.clone())),
            _ => None,
        };
        let h = 56.;
        h_flex()
            .id(SharedString::from(format!("all-row-{}", t.id)))
            .w_full()
            .min_w_0()
            .h(theme::z(h))
            .px(theme::z(14.))
            .gap(theme::z(14.))
            .items_center()
            .rounded(theme::z(theme::by_height(h)))
            .cursor_pointer()
            .hover(|d| d.bg(p.hover))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.stage_pick = Some(id.clone());
                this.stage_shot = 0;
                this.stage_swatch = None;
                cx.notify();
            }))
            .test_support()
            .child(div().w(theme::z(14.)).flex_shrink_0().flex().justify_center().child(mark))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap(theme::z(2.))
                    .child(div().truncate().set(Type::Body).font_weight(FontWeight::MEDIUM).text_color(p.label).child(t.title.clone()))
                    .child(div().truncate().set(Type::Subheadline).text_color(tone).child(line)),
            )
            .when(added + removed > 0, |d| {
                d.child(
                    h_flex()
                        .flex_shrink_0()
                        .gap(theme::z(6.))
                        .set(Type::Subheadline)
                        .font_features(theme::tabular())
                        .child(div().text_color(p.finished).child(format!("+{added}")))
                        .child(div().text_color(p.failed).child(format!("−{removed}"))),
                )
            })
            .when_some(link, |d, (label, url)| {
                d.child(
                    h_flex()
                        .id(SharedString::from(format!("all-link-{}", t.id)))
                        .flex_shrink_0()
                        .h(theme::z(26.))
                        .px(theme::z(10.))
                        .gap(theme::z(5.))
                        .items_center()
                        .rounded(theme::z(theme::by_height(26.)))
                        .border_1()
                        .border_color(p.separator)
                        .set(Type::Subheadline)
                        .font_features(theme::tabular())
                        .text_color(p.label)
                        .cursor_pointer()
                        .hover(|d| d.bg(p.hover))
                        .on_click(move |_, _, cx| {
                            cx.stop_propagation();
                            cx.open_url(&url);
                        })
                        .child(label)
                        .child(Icon::empty().path("icons/arrow-up-right.svg").size(theme::z(11.)).text_color(p.tertiary)),
                )
            })
            .child(div().w(theme::z(64.)).flex_shrink_0().text_right().set(Type::Subheadline).text_color(p.tertiary).child(crate::model::ago(t.updated_ms, now)))
            .into_any_element()
    }

    fn work_card(&self, t: &Task, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let id = t.id.clone();
        let doing = self.stage_doing(t);
        let feed = t.session.and_then(|s| self.feed_el(s, 6));
        let pad = 16.;
        v_flex()
            .id(SharedString::from(format!("all-task-{}", t.id)))
            .flex_basis(relative(0.48))
            .flex_grow(1.)
            .min_w(theme::z(320.))
            .h(theme::z(232.))
            .p(theme::z(pad))
            .gap(theme::z(10.))
            .rounded(theme::z(theme::outer(theme::by_height(26.), pad)))
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
                    .gap(theme::z(3.))
                    .child(div().truncate().font_weight(FontWeight::SEMIBOLD).set(Type::Body).text_color(p.label).child(t.title.clone()))
                    .child(h_flex().gap(theme::z(8.)).items_center().child(div().w(theme::z(12.)).flex().justify_center().child(super::status_dot(p.info, true, &p))).child(div().flex_1().min_w_0().truncate().set(Type::Subheadline).text_color(p.secondary).child(doing))),
            )
            .child(div().h(px(1.)).w_full().bg(p.separator))
            .child(match feed {
                Some(f) => div().flex_1().min_h_0().flex().flex_col().justify_end().child(f).into_any_element(),
                None => div().set(Type::Subheadline).text_color(p.tertiary).child("Starting…").into_any_element(),
            })
            .into_any_element()
    }
}
