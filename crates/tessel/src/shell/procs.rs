//! the processes module: what agents started that's still running. dev
//! servers, watchers, headless browsers and the like, grouped by chat, one row
//! per tree (a browser's helpers fold into it), with what each costs and the
//! ports it holds. the ones nobody will stop (their agent has gone) are
//! marked, and can be stopped one at a time or all at once.
//!
//! the daemon finds them (see tesseld's `procs`); nothing is asked while no
//! processes pane is on screen.

use super::*;
use crate::theme::{ButtonKind, z};
use crate::ui::Module;
use gpui_kit::component::Icon;
use tessel_proto::procs::{AgentProcess, ProcessKind};
use tessel_proto::{Request, Response};

/// how often a visible pane asks again.
const EVERY: Duration = Duration::from_secs(3);

#[derive(Default)]
pub(crate) struct ProcsView {
    rows: Vec<AgentProcess>,
    asking: bool,
    fetched: Option<Instant>,
    loaded: bool,
    error: Option<String>,
    said: Option<String>,
    /// asked to stop this once: the next click does (`-1`: all that were left behind).
    confirm: Option<i32>,
}

fn bytes(b: u64) -> String {
    let mb = b as f64 / 1_048_576.0;
    if mb >= 1024.0 { format!("{:.1} GB", mb / 1024.0) } else { format!("{mb:.0} MB") }
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => format!("{secs}s"),
        60..3600 => format!("{}m", secs / 60),
        3600..172_800 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

impl Shell {
    pub(crate) fn procs_visible(&self) -> bool {
        self.ui.panes().into_iter().any(|p| p.module == Some(Module::Processes) && self.pane_open(p))
    }

    /// every second: the list again when it's due and a pane shows it.
    pub(crate) fn procs_tick(&mut self) {
        if !self.procs_visible() || self.procs.asking || self.procs.fetched.is_some_and(|t| t.elapsed() < EVERY) {
            return;
        }
        self.procs.asking = true;
        self.cmd(Cmd::Ask { tag: "procs.list", req: Request::Processes });
    }

    pub(crate) fn procs_answer(&mut self, tag: &'static str, result: Result<Response, String>, cx: &mut Context<Self>) {
        match (tag, result) {
            ("procs.list", result) => {
                self.procs.asking = false;
                self.procs.fetched = Some(Instant::now());
                self.procs.loaded = true;
                match result {
                    Ok(Response::Processes { procs }) => {
                        self.procs.rows = procs;
                        self.procs.error = None;
                    }
                    Ok(_) => {}
                    Err(e) => self.procs.error = Some(e),
                }
            }
            ("procs.stop", Ok(Response::Shipped { said, .. })) => {
                self.procs.said = Some(said);
                // see it go.
                self.procs.fetched = None;
            }
            ("procs.stop", Err(e)) => self.procs.said = Some(e),
            _ => {}
        }
        cx.notify();
    }

    fn procs_stop(&mut self, pid: i32, cx: &mut Context<Self>) {
        self.procs.confirm = None;
        self.cmd(Cmd::Ask { tag: "procs.stop", req: Request::ProcessStop { pid, force: false } });
        cx.notify();
    }

    fn procs_stop_left(&mut self, cx: &mut Context<Self>) {
        self.procs.confirm = None;
        let left: Vec<i32> = self.procs.rows.iter().filter(|r| r.left_behind).map(|r| r.pid).collect();
        for pid in left {
            self.cmd(Cmd::Ask { tag: "procs.stop", req: Request::ProcessStop { pid, force: false } });
        }
        cx.notify();
    }

    pub(crate) fn procs_el(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let rows = &self.procs.rows;
        let left = rows.iter().filter(|r| r.left_behind).count();
        let browsers = rows.iter().filter(|r| r.kind == ProcessKind::Browser).count();
        let total: u64 = rows.iter().map(|r| r.rss_bytes).sum();
        let mut summary = vec![format!("{} running", rows.len())];
        if left > 0 {
            summary.push(format!("{left} left behind"));
        }
        if browsers > 0 {
            summary.push(format!("{browsers} browser{}", if browsers == 1 { "" } else { "s" }));
        }
        if total > 0 {
            summary.push(bytes(total));
        }
        let sure = self.procs.confirm == Some(-1);
        let bar = h_flex()
            .flex_shrink_0()
            .px(z(12.))
            .py(z(8.))
            .gap(z(8.))
            .border_b_1()
            .border_color(p.separator)
            .child(div().flex_1().min_w_0().truncate().set(Type::Callout).text_color(p.secondary).child(summary.join(" · ")))
            .when(left > 0, |d| {
                d.child(
                    theme::button(&p, "procs-stop-left", if sure { format!("Stop all {left}") } else { "Stop all left behind".to_owned() }, Some("octagon-x"), if sure { ButtonKind::Primary } else { ButtonKind::Ghost })
                        .test_support()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.procs.confirm == Some(-1) {
                                this.procs_stop_left(cx);
                            } else {
                                this.procs.confirm = Some(-1);
                                cx.notify();
                            }
                        })),
                )
            });
        let body: AnyElement = if !self.procs.loaded {
            v_flex().flex_1().items_center().justify_center().child(div().set(Type::Callout).text_color(p.tertiary).child("Looking…")).into_any_element()
        } else if let Some(e) = self.procs.error.as_ref().filter(|_| rows.is_empty()) {
            v_flex().flex_1().p(z(24.)).items_center().justify_center().child(div().set(Type::Callout).text_color(p.failed).child(e.clone())).into_any_element()
        } else if rows.is_empty() {
            v_flex()
                .flex_1()
                .items_center()
                .justify_center()
                .gap(z(8.))
                .child(Icon::empty().path("icons/activity.svg").size(z(20.)).text_color(p.tertiary))
                .child(div().set(Type::Callout).text_color(p.tertiary).child("Nothing an agent started is running."))
                .into_any_element()
        } else {
            // by chat: the ones left behind first.
            let mut groups: Vec<(Option<SessionId>, Vec<&AgentProcess>)> = Vec::new();
            for r in rows {
                match groups.iter_mut().find(|(s, _)| *s == r.session) {
                    Some((_, g)) => g.push(r),
                    None => groups.push((r.session, vec![r])),
                }
            }
            let mut list = v_flex().id("procs-rows").flex_1().min_h_0().overflow_y_scroll().py(z(4.));
            for (session, group) in groups {
                let name = session.and_then(|s| self.model.summary(s)).map_or_else(|| "A chat that's gone".to_owned(), |s| s.goal.clone());
                list = list.child(
                    h_flex()
                        .px(z(12.))
                        .pt(z(10.))
                        .pb(z(4.))
                        .gap(z(6.))
                        .set(Type::Subheadline)
                        .child(div().min_w_0().truncate().font_weight(FontWeight::SEMIBOLD).text_color(p.secondary).child(name))
                        .child(div().flex_shrink_0().text_color(p.quaternary).child(group.len().to_string())),
                );
                for r in group {
                    list = list.child(self.proc_row(r, cx));
                }
            }
            list.into_any_element()
        };
        v_flex()
            .size_full()
            .child(bar)
            .children(self.procs.said.as_ref().map(|s| div().px(z(12.)).py(z(6.)).set(Type::Subheadline).text_color(p.secondary).child(s.clone())))
            .child(body)
            .into_any_element()
    }

    fn proc_row(&self, r: &AgentProcess, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let pid = r.pid;
        let dot = if r.left_behind {
            p.blocked
        } else if r.kind == ProcessKind::Terminal {
            p.tertiary
        } else {
            p.finished
        };
        let sure = self.procs.confirm == Some(pid);
        let group = SharedString::from(format!("proc-{pid}"));
        let small = |s: String, w: f32, ink: Hsla| div().w(z(w)).flex_shrink_0().text_right().set(Type::Subheadline).text_color(ink).child(s);
        let mut what = vec![];
        if r.kind == ProcessKind::Browser {
            what.push("browser".to_owned());
        }
        if r.children > 0 {
            what.push(format!("{} processes", r.children + 1));
        }
        if r.left_behind {
            what.push("left behind".to_owned());
        }
        h_flex()
            .id(group.clone())
            .test_support()
            .group(group.clone())
            .mx(z(4.))
            .px(z(8.))
            .py(z(5.))
            .gap(z(8.))
            .rounded(z(theme::R_TILE))
            .hover(|d| d.bg(p.hover))
            .tooltip({
                let command = r.command.clone();
                move |w, cx| gpui_kit::component::tooltip::Tooltip::new(command.clone()).build(w, cx)
            })
            .child(div().size(z(7.)).flex_shrink_0().rounded_full().bg(dot))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap(z(6.))
                            .min_w_0()
                            .child(div().min_w_0().truncate().set(Type::Body).text_color(p.label).child(r.name.clone()))
                            .children(r.ports.iter().take(3).map(|port| div().flex_shrink_0().px(z(5.)).rounded(z(4.)).bg(p.fill).set(Type::Code).text_color(p.secondary).child(format!(":{port}")))),
                    )
                    .when(!what.is_empty(), |d| d.child(div().set(Type::Subheadline).text_color(if r.left_behind { p.blocked } else { p.tertiary }).child(what.join(" · ")))),
            )
            .child(small(format!("{:.0}%", r.cpu), 36., p.secondary))
            .child(small(bytes(r.rss_bytes), 64., p.secondary))
            .child(small(age(r.age_secs), 32., p.tertiary))
            .child(
                div().w(z(64.)).flex_shrink_0().flex().justify_end().child(
                    theme::button(&p, SharedString::from(format!("proc-{pid}-stop")), if sure { "Stop it" } else { "Stop" }, None, if sure { ButtonKind::Primary } else { ButtonKind::Ghost })
                        .test_support()
                        .when(!sure, |b| b.invisible().group_hover(group.clone(), |s| s.visible()))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.procs.confirm == Some(pid) {
                                this.procs_stop(pid, cx);
                            } else {
                                this.procs.confirm = Some(pid);
                                cx.notify();
                            }
                        })),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_and_ages_read_short() {
        assert_eq!(super::bytes(1_200 * 1_048_576), "1.2 GB");
        assert_eq!(super::bytes(300 * 1_048_576), "300 MB");
        assert_eq!(super::age(4000), "1h");
    }
}
