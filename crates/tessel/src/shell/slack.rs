use super::*;
use tessel_proto::{Request, Response};

pub(crate) const SINCE: [(&str, &str); 4] = [("last", "New since last time"), ("today", "Today"), ("7d", "Last 7 days"), ("30d", "Last 30 days")];

#[derive(Clone, Debug)]
pub(crate) struct SlackRun {
    pub read: u32,
    pub filed: usize,
    pub skipped: u32,
    pub channel: String,
}

pub(crate) fn since_unix(choice: &str, now: chrono::DateTime<chrono::Local>) -> Option<i64> {
    match choice {
        "today" => now.date_naive().and_hms_opt(0, 0, 0).and_then(|d| d.and_local_timezone(chrono::Local).single()).map(|d| d.timestamp()),
        "7d" => Some(now.timestamp() - 7 * 86_400),
        "30d" => Some(now.timestamp() - 30 * 86_400),
        _ => None,
    }
}

impl Shell {
    pub(crate) fn want_slack(&mut self) {
        if self.slack_channels.is_none() {
            self.slack_channels = Some(Ok(Vec::new()));
            self.cmd(Cmd::Ask { tag: "slack.channels", req: Request::SlackChannels });
        }
    }

    pub(crate) fn slack_channels_answer(&mut self, result: Result<Response, String>) {
        self.slack_channels = Some(match result {
            Ok(Response::SlackChannels { channels }) => {
                if self.slack_channel.is_none() {
                    self.slack_channel = channels.first().map(|c| c.id.clone());
                }
                Ok(channels)
            }
            Ok(_) => Ok(Vec::new()),
            Err(e) => Err(e),
        });
    }

    pub(crate) fn slack_read_answer(&mut self, result: Result<Response, String>) {
        self.slack_run = Some(Some(match result {
            Ok(Response::SlackRead { read, filed, skipped, channel }) => Ok(SlackRun { read, filed: filed.len(), skipped, channel }),
            Ok(_) => Err("Slack didn't answer".into()),
            Err(e) => Err(e),
        }));
    }

    fn read_slack(&mut self, repo: String, ready: bool, cx: &mut Context<Self>) {
        let Some(channel) = self.slack_channel.clone() else { return };
        let note = self.slack_note.read(cx).value().trim().to_owned();
        let since = since_unix(self.slack_since, chrono::Local::now());
        self.slack_run = Some(None);
        self.cmd(Cmd::Ask { tag: "slack.read", req: Request::SlackRead { repo, channel, since, note, ready } });
        cx.notify();
    }

    pub(crate) fn slack_card(&self, repo: &str, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let quiet = |t: String, c: Hsla| div().set(Type::Body).text_color(c).child(t);
        let channels = match &self.slack_channels {
            Some(Ok(c)) if !c.is_empty() => c.clone(),
            Some(Err(e)) => return quiet(e.clone(), p.secondary).into_any_element(),
            Some(Ok(_)) => return quiet("Invite the Tessel app to a channel with /invite @Tessel, then it shows up here.".into(), p.secondary).into_any_element(),
            None => return quiet("Asking Slack…".into(), p.tertiary).into_any_element(),
        };
        let current = self.slack_channel.clone().unwrap_or_default();
        let label = channels.iter().find(|c| c.id == current).map_or_else(|| "Choose a channel".to_owned(), |c| format!("#{}", c.name));
        let channel = self.select(
            super::select::Select { id: "slack-channel", label, options: channels.iter().map(|c| (c.id.clone(), format!("#{}", c.name))).collect(), current, width: Some(200.), height: 34., up: false },
            std::rc::Rc::new(|this: &mut Shell, value: &str, _: &mut Window, _: &mut Context<Shell>| this.slack_channel = Some(value.to_owned())),
            cx,
        );
        let since_label = SINCE.iter().find(|(v, _)| *v == self.slack_since).map_or("New since last time", |(_, l)| l).to_owned();
        let since = self.select(
            super::select::Select { id: "slack-since", label: since_label, options: SINCE.iter().map(|(v, l)| ((*v).to_owned(), (*l).to_owned())).collect(), current: self.slack_since.to_owned(), width: Some(200.), height: 34., up: false },
            std::rc::Rc::new(|this: &mut Shell, value: &str, _: &mut Window, _: &mut Context<Shell>| this.slack_since = SINCE.iter().find(|(v, _)| *v == value).map_or("last", |(v, _)| v)),
            cx,
        );
        let busy = matches!(self.slack_run, Some(None));
        let result = match &self.slack_run {
            Some(Some(Ok(r))) => Some((
                if r.filed == 0 {
                    format!("Read {} message{} in #{} and found nothing new to do", r.read, if r.read == 1 { "" } else { "s" }, r.channel)
                } else {
                    format!("Read {} message{} in #{} and filed {} task{}", r.read, if r.read == 1 { "" } else { "s" }, r.channel, r.filed, if r.filed == 1 { "" } else { "s" })
                } + &if r.skipped > 0 { format!(", skipping {} already filed", r.skipped) } else { String::new() },
                p.secondary,
            )),
            Some(Some(Err(e))) => Some((e.clone(), p.failed)),
            Some(None) => Some(("Reading the channel…".to_owned(), p.secondary)),
            None => None,
        };
        let (r1, r2) = (repo.to_owned(), repo.to_owned());
        let field_h = 34.;
        v_flex()
            .id("slack-card")
            .w_full()
            .gap(theme::z(10.))
            .child(h_flex().w_full().gap(theme::z(8.)).child(channel).child(since))
            .child(
                h_flex()
                    .w_full()
                    .h(theme::z(field_h))
                    .px(theme::z(12.))
                    .items_center()
                    .rounded(theme::z(theme::by_height(field_h)))
                    .border_1()
                    .border_color(p.separator)
                    .set(Type::Body)
                    .child(div().w_full().child(Input::new(&self.slack_note).id("slack-note").appearance(false))),
            )
            .child(
                h_flex()
                    .w_full()
                    .gap(theme::z(8.))
                    .items_center()
                    .child(div().flex_1().min_w_0().children(result.map(|(t, c)| div().flex().items_center().gap(theme::z(8.)).when(busy, |d| d.child(super::live_dot(p.info, &p))).child(div().truncate().set(Type::Subheadline).text_color(c).child(t)))))
                    .when(!busy, |d| {
                        d.child(
                            theme::button(&p, "slack-drafts", "Save as Drafts", None, theme::ButtonKind::Ghost)
                                .h(theme::z(32.))
                                .rounded(theme::z(theme::by_height(32.)))
                                .on_click(cx.listener(move |this, _, _, cx| this.read_slack(r1.clone(), false, cx)))
                                .test_support(),
                        )
                        .child(
                            theme::button(&p, "slack-read", "Read and Start", None, theme::ButtonKind::Secondary)
                                .h(theme::z(32.))
                                .rounded(theme::z(theme::by_height(32.)))
                                .on_click(cx.listener(move |this, _, _, cx| this.read_slack(r2.clone(), true, cx)))
                                .test_support(),
                        )
                    }),
            )
            .into_any_element()
    }
}
