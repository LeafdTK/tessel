use super::Shell;
use super::panel::Tab;
use crate::bridge::Cmd;
use crate::theme::{self, Type, Typeset as _};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use tessel_proto::proof::{Proof, Severity, Verdict};
use tessel_proto::{Request, SessionId, ShipStep};

const TEACH: &str = "Teach this repository something to hold every change to, from what I say below. Decide whether it's a rule (something every change must follow) or knowledge (how part of the system works, why, and what's allowed). Ask me with tessel_ask only if something important is unclear. Write the file under .tessel/, then tell me in a line what you added.";

const FROM_FINDING: &str = "A review of a change found the problem below. Make it a house rule for this repository, so every change is checked for it from now on: say it more generally than this one case, and give a short good and bad example. Write it under .tessel/rules/, then tell me in a line what you added.";

const FROM_REVIEW: &str = "In a review of an agent's change I asked for the change below. If it says something every change in this repository should follow, make it a house rule: say it generally, with a short good and bad example, and write it under .tessel/rules/. If it was only about that one change, write nothing and say so in a line. Otherwise tell me in a line what you added.";

const SET_UP: &str = "Set up house rules for this repository, so tessel can prove changes against them. Read what's here first: AGENTS.md, CLAUDE.md, .cursor/rules, the CI config, the build and test scripts, and the last 50 commit subjects. Then work out: verify commands for .tessel/settings.toml that are fast and reliable (a lint, a type check, the quick tests), the 3 to 8 rules that matter most here, and knowledge files for anything a reviewer couldn't guess. Show me the list with tessel_ask (one multiselect, everything ticked) before writing anything. Write what I keep, then tell me in a line what you added.";

pub(crate) fn teach_prompt(typed: &str) -> String {
    format!("{TEACH}\n\nWhat to remember: {typed}\n\n{}", tessel_proto::proof::RULE_FORMAT)
}

pub(crate) fn verdict_word(p: &Proof) -> String {
    match p.verdict {
        Verdict::Proving => "Proving…".into(),
        Verdict::Proven => "Proven".into(),
        Verdict::Concerns => "Concerns".into(),
        Verdict::Failed => match p.blocking() {
            1 => "1 blocking".into(),
            n => format!("{n} blocking"),
        },
        Verdict::Broken => "Couldn't prove".into(),
    }
}

impl Shell {
    pub(crate) fn factory_on(&self) -> bool {
        self.ui.factory.unwrap_or(false)
    }

    pub(crate) fn prove(&mut self, session: SessionId, anyway: bool, cx: &mut Context<Self>) {
        self.cmd(Cmd::Ask { tag: "proof.run", req: Request::ProofRun { session, anyway } });
        cx.notify();
    }

    pub(crate) fn rules_answer(&mut self, result: Result<tessel_proto::Response, String>) {
        if let (Ok(tessel_proto::Response::Rules { rulebook }), Some(id)) = (result, self.model.open.as_ref().map(|o| o.id)) {
            self.rulebook = Some((id, rulebook));
        }
    }

    fn repo_root(&self) -> Option<String> {
        let s = self.model.open_summary()?;
        Some(s.worktree.as_ref().map_or_else(|| s.cwd.clone(), |w| w.repo.clone()))
    }

    pub(crate) fn teach(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else { return };
        self.authoring = false;
        self.teaching = true;
        self.start_in(root, window, cx);
    }

    pub(crate) fn set_up_rules(&mut self, cx: &mut Context<Self>) {
        let Some(cwd) = self.repo_root() else { return };
        let prompt = format!("{SET_UP}\n\n{}", tessel_proto::proof::RULE_FORMAT);
        self.cmd(Cmd::Create { cwd, worktree: false, prompt, attachments: Vec::new(), model: Some("sonnet".into()) });
        cx.notify();
    }

    pub(crate) fn rule_from(&mut self, finding: &tessel_proto::proof::Finding, cx: &mut Context<Self>) {
        let Some(cwd) = self.repo_root() else { return };
        let at = match (&finding.file, finding.line) {
            (Some(f), Some(l)) => format!(" ({f}:{l})"),
            (Some(f), None) => format!(" ({f})"),
            _ => String::new(),
        };
        let prompt = format!("{FROM_FINDING}\n\nThe finding{at}: {}\n{}\n\n{}", finding.title, finding.why, tessel_proto::proof::RULE_FORMAT);
        self.cmd(Cmd::Create { cwd, worktree: false, prompt, attachments: Vec::new(), model: Some("sonnet".into()) });
        cx.notify();
    }

    pub(crate) fn teach_from_review(&mut self, repo: String, said: String, cx: &mut Context<Self>) {
        let prompt = format!("{FROM_REVIEW}\n\nWhat I asked for in review: {said}\n\n{}", tessel_proto::proof::RULE_FORMAT);
        self.cmd(Cmd::Create { cwd: repo, worktree: false, prompt, attachments: Vec::new(), model: Some("sonnet".into()) });
        cx.notify();
    }

    pub(crate) fn teaching_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.teaching {
            return None;
        }
        let p = self.palette.get();
        Some(
            h_flex()
                .id("teaching-chip")
                .test_support()
                .gap_2()
                .px_2()
                .h(px(28.))
                .rounded(px(theme::R_TILE))
                .bg(p.accent.opacity(0.12))
                .text_color(p.accent)
                .child(Icon::empty().path("icons/badge-check.svg").size(theme::z(13.)))
                .child(div().set(Type::Callout).font_weight(FontWeight::MEDIUM).child("Teach this repository: say what every change should follow"))
                .child(Button::new("teaching-cancel").xsmall().ghost().icon(Icon::empty().path("icons/x.svg")).on_click(cx.listener(|this, _, _, cx| {
                    this.teaching = false;
                    cx.notify();
                })))
                .into_any_element(),
        )
    }

    pub(crate) fn house_rules_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.factory_on() {
            return None;
        }
        let s = self.model.open_summary()?;
        let p = self.palette.get();
        let book = self.rulebook.as_ref().filter(|(id, _)| *id == s.id).map(|(_, b)| b);
        let mut col = v_flex().child(
            h_flex()
                .pr_3()
                .items_end()
                .child(self.section("House Rules").flex_1())
                .child(Button::new("rules-teach").xsmall().ghost().label("Teach").on_click(cx.listener(|this, _, window, cx| this.teach(window, cx)))),
        );
        let Some(book) = book else { return Some(col) };
        let own: Vec<_> = book.rules.iter().filter(|r| !r.builtin).collect();
        if own.is_empty() && book.knowledge.is_empty() {
            col = col.child(
                h_flex()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().set(Type::Subheadline).text_color(p.secondary).child("Only tessel's own checks so far."))
                    .child(Button::new("rules-set-up").xsmall().outline().label("Set Up").on_click(cx.listener(|this, _, _, cx| this.set_up_rules(cx)))),
            );
        }
        let row = |id: String, title: String, tag: String, path: String, cx: &mut Context<Self>| {
            h_flex()
                .id(ElementId::from(SharedString::from(id)))
                .test_support()
                .h(px(24.))
                .mx_1()
                .px_2()
                .gap_2()
                .rounded(px(theme::R_TILE))
                .hover(|d| d.bg(p.hover))
                .on_click(cx.listener(move |this, _, _, cx| this.open_file(path.clone(), cx)))
                .child(div().flex_1().min_w_0().set(Type::Callout).text_color(p.label).truncate().child(title))
                .child(div().flex_shrink_0().set(Type::Subheadline).text_color(p.tertiary).child(tag))
        };
        for r in own {
            let kind = match &r.check {
                tessel_proto::proof::Check::Run { .. } => "command",
                tessel_proto::proof::Check::Match { .. } => "pattern",
                _ => "reviewed",
            };
            let tag = if r.holdout { format!("{kind} · holdout") } else { kind.to_owned() };
            col = col.child(row(format!("rule-{}", r.id), r.title.clone(), tag, r.path.clone(), cx));
        }
        for k in &book.knowledge {
            col = col.child(row(format!("knowledge-{}", k.id), k.title.clone(), "knowledge".into(), k.path.clone(), cx));
        }
        for problem in &book.problems {
            col = col.child(div().px_3().set(Type::Subheadline).text_color(p.failed).child(problem.clone()));
        }
        Some(col)
    }

    pub(crate) fn proof_answer(&mut self, result: Result<tessel_proto::Response, String>) {
        if let Err(e) = result {
            self.error = Some(e);
        }
    }

    pub(crate) fn show_checks(&mut self, cx: &mut Context<Self>) {
        self.ws.tab = Tab::Checks;
        if !self.panel_visible() {
            self.toggle_inspector(cx);
        }
        self.refresh_workspace();
        cx.notify();
    }

    pub(crate) fn ship_anyway(&mut self, cx: &mut Context<Self>) {
        let (Some(session), Some(step)) = (self.model.open.as_ref().map(|o| o.id), self.last_ship.clone()) else { return };
        self.shipping = true;
        self.shipped = None;
        self.cmd(Cmd::Ask { tag: "ship", req: Request::Ship { session, step, force: true } });
        cx.notify();
    }

    pub(crate) fn remember_ship(&mut self, step: &ShipStep) {
        self.last_ship = Some(step.clone());
    }

    pub(crate) fn send_findings(&mut self, cx: &mut Context<Self>) {
        let Some(s) = self.model.open_summary() else { return };
        let Some(proof) = &s.proof else { return };
        let text = format!(
            "Proving your change found the problems below. Fix the ones that are real, say which you think aren't and why, then prove it again with tessel_prove.\n\n{}",
            proof.report()
        );
        let session = s.id;
        self.cmd(Cmd::Send { session, text, attachments: Vec::new() });
        cx.notify();
    }

    pub(crate) fn proof_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let s = self.model.open_summary()?;
        let proof = s.proof.as_ref()?;
        if self.show_new {
            return None;
        }
        let p = self.palette.get();
        let ink = match proof.verdict {
            Verdict::Failed | Verdict::Broken => p.failed,
            Verdict::Concerns => p.blocked,
            _ => p.secondary,
        };
        Some(
            h_flex()
                .id("proof-chip")
                .test_support()
                .aria_label(SharedString::from(format!("Proof: {}", verdict_word(proof))))
                .h(px(22.))
                .px(px(8.))
                .gap(px(5.))
                .rounded(px(11.))
                .border_1()
                .border_color(p.separator)
                .hover(|d| d.bg(p.hover))
                .on_click(cx.listener(|this, _, _, cx| this.show_checks(cx)))
                .when(proof.verdict == Verdict::Proving, |d| d.child(super::live_dot(p.blocked, &p)))
                .when(proof.verdict != Verdict::Proving, |d| d.child(Icon::empty().path("icons/badge-check.svg").size(theme::z(12.)).text_color(ink)))
                .child(div().set(Type::Subheadline).text_color(ink).child(verdict_word(proof)))
                .when(proof.verdict.done() && proof.cost_usd >= 0.005, |d| d.child(div().set(Type::Subheadline).text_color(p.tertiary).font_features(theme::tabular()).child(format!("${:.2}", proof.cost_usd))))
                .into_any_element(),
        )
    }

    pub(crate) fn proof_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        let s = self.model.open_summary()?;
        if !self.factory_on() && s.proof.is_none() {
            return None;
        }
        let p = self.palette.get();
        let session = s.id;
        let proving = s.proof.as_ref().is_some_and(|x| x.verdict == Verdict::Proving);
        let dim = |text: String| div().px_3().set(Type::Subheadline).text_color(p.secondary).child(text);
        let mut col = v_flex().child(
            h_flex().pr_3().items_end().child(self.section("Proof").flex_1()).child(
                Button::new("proof-run")
                    .xsmall()
                    .ghost()
                    .label(if s.proof.is_some() { "Prove Again" } else { "Prove" })
                    .disabled(proving)
                    .on_click(cx.listener(move |this, _, _, cx| this.prove(session, false, cx))),
            ),
        );
        let Some(proof) = &s.proof else { return Some(col.child(dim("Not proved yet.".into()))) };
        if proving {
            return Some(col.child(
                h_flex().px_3().gap(px(8.)).items_center().set(Type::Callout).text_color(p.secondary).child(super::live_dot(p.blocked, &p)).child(if proof.stage.is_empty() { "Proving…".to_owned() } else { format!("{}…", proof.stage) }),
            ));
        }
        let ink = match proof.verdict {
            Verdict::Failed | Verdict::Broken => p.failed,
            Verdict::Concerns => p.blocked,
            _ => p.label,
        };
        let secs = proof.finished_ms.unwrap_or(proof.started_ms).saturating_sub(proof.started_ms) / 1000;
        let took = if secs >= 60 { format!("{}m {}s", secs / 60, secs % 60) } else { format!("{secs}s") };
        col = col
            .child(div().px_3().set(Type::Callout).text_color(ink).child(verdict_word(proof)))
            .child(dim(format!("{} files · {took} · ${:.2}{}", proof.files, proof.cost_usd, if proof.overridden { " · shipped anyway" } else { "" })));
        if let Some(why) = &proof.problem {
            col = col.child(dim(why.clone()));
        }
        if proof.skipped {
            col = col.child(h_flex().px_3().pt_1().child(Button::new("proof-anyway").small().outline().label("Review Anyway").on_click(cx.listener(move |this, _, _, cx| this.prove(session, true, cx)))));
        }
        for (ix, v) in proof.verify.iter().enumerate() {
            col = col.child(
                h_flex()
                    .id(("proof-verify", ix))
                    .h(px(24.))
                    .mx_1()
                    .px_2()
                    .gap_2()
                    .child(div().w(px(12.)).flex_shrink_0().set(Type::Callout).text_color(if v.ok { p.secondary } else { p.failed }).child(if v.ok { "✓" } else { "✕" }))
                    .child(div().flex_1().overflow_hidden().set(Type::Code).text_color(p.label).truncate().child(v.command.clone())),
            );
            if !v.ok && !v.output.is_empty() {
                let tail: Vec<&str> = v.output.lines().rev().take(14).collect::<Vec<_>>().into_iter().rev().collect();
                col = col.child(div().mx_3().mb_2().p_2().rounded(px(theme::R_TILE)).bg(p.fill).set(Type::Code).text_color(p.secondary).child(tail.join("\n")));
            }
        }
        let standing: Vec<_> = proof.standing().collect();
        for (ix, f) in standing.iter().enumerate() {
            let (glyph, color) = match f.severity {
                Severity::Block => ("●", p.failed),
                Severity::Warn => ("●", p.blocked),
                Severity::Note => ("○", p.tertiary),
            };
            let at = match (&f.file, f.line) {
                (Some(file), Some(l)) => Some(format!("{file}:{l}")),
                (Some(file), None) => Some(file.clone()),
                _ => None,
            };
            let file = f.file.clone();
            let teach = (*f).clone();
            col = col.child(
                v_flex()
                    .id(ElementId::from(SharedString::from(format!("proof-finding-{ix}"))))
                    .test_support()
                    .aria_label(SharedString::from(f.title.clone()))
                    .mx_1()
                    .px_2()
                    .py_1()
                    .gap(px(2.))
                    .rounded(px(theme::R_TILE))
                    .when(file.is_some(), |d| d.hover(|d| d.bg(p.hover)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(f) = &file {
                            this.open_file(f.clone(), cx);
                        }
                    }))
                    .child(
                        h_flex()
                            .gap_2()
                            .items_start()
                            .child(div().w(px(12.)).flex_shrink_0().set(Type::Callout).text_color(color).child(glyph))
                            .child(div().flex_1().min_w_0().set(Type::Callout).text_color(p.label).child(f.title.clone()))
                            .when_some(f.rule.clone(), |d, r| d.child(div().flex_shrink_0().px(px(6.)).rounded(px(8.)).bg(p.fill).set(Type::Subheadline).text_color(p.secondary).child(r)))
                            .when(f.rule.is_none(), |d| {
                                d.child(
                                    Button::new(ElementId::from(SharedString::from(format!("proof-rule-{ix}"))))
                                        .xsmall()
                                        .ghost()
                                        .label("Make a Rule")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            cx.stop_propagation();
                                            this.rule_from(&teach, cx);
                                        })),
                                )
                            }),
                    )
                    .when_some(at, |d, at| d.child(div().pl(px(20.)).set(Type::Code).text_color(p.tertiary).truncate().child(at)))
                    .when(!f.why.is_empty(), |d| d.child(div().pl(px(20.)).set(Type::Subheadline).text_color(p.secondary).child(f.why.clone()))),
            );
        }
        let dropped = proof.findings.len() - standing.len();
        if dropped > 0 {
            col = col.child(dim(match dropped {
                1 => "1 more didn't hold up on a second look.".into(),
                n => format!("{n} more didn't hold up on a second look."),
            }));
        }
        for r in proof.reviewers.iter().filter(|r| r.error.is_some()) {
            col = col.child(dim(format!("The {} reviewer couldn't finish: {}", r.lens, r.error.as_deref().unwrap_or_default())));
        }
        if !standing.is_empty() || proof.verify.iter().any(|v| !v.ok) {
            col = col.child(
                h_flex().px_3().pt_2().child(Button::new("proof-send").small().outline().label("Send to the Agent").on_click(cx.listener(|this, _, _, cx| this.send_findings(cx)))),
            );
        }
        Some(col)
    }
}
