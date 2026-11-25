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

pub(crate) fn mentioned<'a>(text: &str, channels: &'a [tessel_proto::SlackChannel]) -> Option<&'a tessel_proto::SlackChannel> {
    let lower = text.to_lowercase();
    let words: Vec<&str> = lower.split(|c: char| !(c.is_alphanumeric() || c == '-' || c == '_' || c == '#')).filter(|w| !w.is_empty()).collect();
    let slacky = lower.contains("slack") || lower.contains("channel");
    channels.iter().find(|c| {
        let name = c.name.to_lowercase();
        words.iter().any(|w| *w == format!("#{name}") || (slacky && *w == name))
    })
}

pub(crate) fn since_in(text: &str, now: chrono::DateTime<chrono::Local>) -> Option<i64> {
    let t = text.to_lowercase();
    let day = 86_400;
    let midnight = |d: chrono::NaiveDate| d.and_hms_opt(0, 0, 0).and_then(|d| d.and_local_timezone(chrono::Local).single()).map(|d| d.timestamp());
    if t.contains("today") {
        return midnight(now.date_naive());
    }
    if t.contains("yesterday") {
        return midnight(now.date_naive() - chrono::Days::new(1));
    }
    if t.contains("this week") {
        use chrono::Datelike as _;
        return midnight(now.date_naive() - chrono::Days::new(now.weekday().num_days_from_monday() as u64));
    }
    if t.contains("last week") || t.contains("past week") {
        return Some(now.timestamp() - 7 * day);
    }
    if t.contains("last month") || t.contains("past month") || t.contains("this month") {
        return Some(now.timestamp() - 30 * day);
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    for w in words.windows(3) {
        if (w[0] == "last" || w[0] == "past")
            && let Ok(n) = w[1].parse::<i64>()
        {
            let unit = w[2].trim_end_matches(|c: char| !c.is_alphabetic());
            let per = match unit {
                "day" | "days" => day,
                "week" | "weeks" => 7 * day,
                "month" | "months" => 30 * day,
                "hour" | "hours" => 3_600,
                _ => continue,
            };
            return Some(now.timestamp() - n * per);
        }
    }
    use chrono::Datelike as _;
    let months = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let on = |m: u32, d: u32, y: Option<i32>| {
        let y = y.unwrap_or(now.year());
        let date = chrono::NaiveDate::from_ymd_opt(y, m, d)?;
        let date = if date > now.date_naive() && y == now.year() { chrono::NaiveDate::from_ymd_opt(y - 1, m, d)? } else { date };
        midnight(date)
    };
    let clean = |w: &str| w.trim_matches(|c: char| !c.is_alphanumeric() && c != '/' && c != '-').to_owned();
    for (i, w) in words.iter().enumerate() {
        let w = clean(w);
        if let Some(m) = months.iter().position(|m| w.len() >= 3 && w.starts_with(m))
            && let Some(d) = words.get(i + 1).map(|n| clean(n).trim_end_matches(|c: char| c.is_alphabetic()).to_owned()).and_then(|n| n.parse::<u32>().ok())
        {
            return on(m as u32 + 1, d, None);
        }
        let parts: Vec<&str> = w.split(['/', '-']).collect();
        let nums: Vec<u32> = parts.iter().filter_map(|x| x.parse().ok()).collect();
        if nums.len() == parts.len() && parts.len() >= 2 {
            match (parts.len(), parts[0].len()) {
                (3, 4) => return on(nums[1], nums[2], Some(nums[0] as i32)),
                (2, _) if w.contains('/') => return on(nums[0], nums[1], None),
                (3, _) if w.contains('/') => return on(nums[0], nums[1], Some(if nums[2] < 100 { 2000 + nums[2] as i32 } else { nums[2] as i32 })),
                _ => {}
            }
        }
    }
    let days = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"];
    for w in words.windows(2) {
        if w[0] == "since"
            && let Some(i) = days.iter().position(|d| w[1].trim_end_matches(|c: char| !c.is_alphabetic()) == *d)
        {
            let back = (now.weekday().num_days_from_monday() as i64 - i as i64).rem_euclid(7);
            let back = if back == 0 { 7 } else { back };
            return midnight(now.date_naive() - chrono::Days::new(back as u64));
        }
    }
    None
}

impl Shell {
    pub(crate) fn slack_mention(&self, text: &str) -> Option<tessel_proto::SlackChannel> {
        if let Some(Ok(channels)) = &self.slack_channels
            && !channels.is_empty()
        {
            return mentioned(text, channels).cloned();
        }
        let name = text.split(|c: char| c.is_whitespace() || c == ',' || c == '.').find_map(|w| w.strip_prefix('#')).filter(|n| n.len() > 1 && !n.chars().all(|c| c.is_ascii_digit()) && n.chars().all(|c| c.is_alphanumeric() || c == '-' || c == '_'))?;
        Some(tessel_proto::SlackChannel { id: name.to_owned(), name: name.to_owned(), private: false })
    }

    pub(crate) fn slack_status(&self) -> Option<AnyElement> {
        let p = self.palette.get();
        let (text, color, busy) = match self.slack_run.as_ref()? {
            None => ("Reading the channel and picking out tasks. This takes about half a minute.".to_owned(), p.secondary, true),
            Some(Ok(r)) => (
                format!("Read {} message{} in {}: filed {} task{}", r.read, if r.read == 1 { "" } else { "s" }, r.channel, r.filed, if r.filed == 1 { "" } else { "s" }) + &if r.skipped > 0 { format!(", skipped {} already filed", r.skipped) } else { String::new() },
                p.finished,
                false,
            ),
            Some(Err(e)) => (e.clone(), p.failed, false),
        };
        Some(
            h_flex()
                .id("slack-status")
                .test_support()
                .w_full()
                .gap(theme::z(8.))
                .items_center()
                .when(busy, |d| d.child(super::live_dot(p.info, &p)))
                .child(div().flex_1().min_w_0().set(Type::Subheadline).text_color(color).child(text))
                .into_any_element(),
        )
    }

    pub(crate) fn want_slack(&mut self) {
        let now = super::now_ms();
        let stale = matches!(self.slack_channels, Some(Err(_))) && now.saturating_sub(self.slack_asked_ms) > 30_000;
        if self.slack_channels.is_none() || stale {
            self.slack_asked_ms = now;
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

#[cfg(test)]
mod tests {
    use super::{mentioned, since_in};
    use chrono::TimeZone as _;

    fn ch(name: &str) -> tessel_proto::SlackChannel {
        tessel_proto::SlackChannel { id: format!("C{name}"), name: name.into(), private: false }
    }

    #[test]
    fn a_channel_is_noticed_by_its_hash_or_by_name_next_to_slack() {
        let list = [ch("orchard-support"), ch("general")];
        assert_eq!(mentioned("go look at #orchard-support and fix what people ask for", &list).map(|c| c.name.as_str()), Some("orchard-support"));
        assert_eq!(mentioned("read the orchard-support slack channel", &list).map(|c| c.name.as_str()), Some("orchard-support"));
        assert!(mentioned("support people better in orchard", &list).is_none());
        assert!(mentioned("fix #42", &list).is_none());
    }

    #[test]
    fn when_to_read_from_is_heard_in_the_words() {
        let now = chrono::Local.with_ymd_and_hms(2026, 11, 26, 15, 0, 0).unwrap();
        let day = 86_400;
        assert_eq!(since_in("anything from the last 3 days", now), Some(now.timestamp() - 3 * day));
        assert_eq!(since_in("what came in last week", now), Some(now.timestamp() - 7 * day));
        assert_eq!(since_in("only today", now), chrono::Local.with_ymd_and_hms(2026, 11, 26, 0, 0, 0).single().map(|d| d.timestamp()));
        assert_eq!(since_in("since monday", now), chrono::Local.with_ymd_and_hms(2026, 11, 23, 0, 0, 0).single().map(|d| d.timestamp()));
        assert_eq!(since_in("just do the tasks", now), None);
        let nov20 = chrono::Local.with_ymd_and_hms(2026, 11, 20, 0, 0, 0).single().map(|d| d.timestamp());
        assert_eq!(since_in("fix what people asked for since fri nov 20", now), nov20);
        assert_eq!(since_in("since November 20th", now), nov20);
        assert_eq!(since_in("from 11/20", now), nov20);
        assert_eq!(since_in("from 2026-11-20 on", now), nov20);
        assert_eq!(since_in("since dec 3", now), chrono::Local.with_ymd_and_hms(2025, 12, 3, 0, 0, 0).single().map(|d| d.timestamp()), "a date that hasn't come yet this year is last year's");
    }
}
