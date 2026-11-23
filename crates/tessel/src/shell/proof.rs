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

    pub(crate) fn prove(&mut self, session: SessionId, cx: &mut Context<Self>) {
        self.cmd(Cmd::Ask { tag: "proof.run", req: Request::ProofRun { session } });
        cx.notify();
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
                    .on_click(cx.listener(move |this, _, _, cx| this.prove(session, cx))),
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
                            .when_some(f.rule.clone(), |d, r| d.child(div().flex_shrink_0().px(px(6.)).rounded(px(8.)).bg(p.fill).set(Type::Subheadline).text_color(p.secondary).child(r))),
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
