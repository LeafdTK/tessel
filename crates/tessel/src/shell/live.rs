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
}
