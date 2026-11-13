//! other machines. the window shows one at a time: this mac, or a dev box
//! (a linux machine with its own daemon, reached over quic). picked at the
//! top of the sidebar; added, set up and kept up to date in Settings › Hosts.
//!
//! switching is a new connection, and everything the window knew about the
//! last machine (its chats, terminals, pages) is let go: none of it is the
//! new one's. the work itself carries on there, as it does when the window
//! closes.

use super::*;
use crate::theme::{ButtonKind, z};
use gpui_kit::component::Icon;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::switch::Switch;
use tessel_remote::hosts::{Host, Hosts, Transport};
use tessel_remote::setup::{Options, Said};

/// the machine card's height, at the top of the sidebar.
pub(crate) const PICKER_H: f32 = 50.0;

#[derive(Default)]
pub(crate) struct HostsView {
    /// tessel's home on this mac, where `hosts.json` and this mac's key are.
    pub(crate) home: PathBuf,
    /// `hosts.json`, as last read.
    pub(crate) list: Hosts,
    /// the machine the window is connected to now: a host's id, or none for this mac.
    pub(crate) on: Option<String>,
    /// how: "this Mac", "quic, 23 ms", "ssh".
    pub(crate) via: String,
    pub(crate) run: Option<SetupRun>,
    pub(crate) form: Option<HostForm>,
    /// the add form is open (its fields are made once the window can).
    pub(crate) adding: bool,
    pub(crate) confirm_remove: Option<String>,
    /// what the last thing done to a host said.
    pub(crate) said: Option<String>,
    /// how sandboxes are reached (`remote.json`).
    pub(crate) net: tessel_remote::net::Net,
    /// your own relay's address, as typed (made when the page shows, inputs need the window).
    pub(crate) relay_input: Option<Entity<InputState>>,
    /// "your own" was picked with no address yet: the field shows, waiting for one.
    pub(crate) relay_own_next: bool,
    /// the setup flow, when it's open.
    pub(crate) flow: Option<super::setup_flow::Flow>,
    /// open the flow at the next frame (asked for where there was no window): for a new box, or this one.
    pub(crate) flow_next: Option<Option<String>>,
}

/// setting a host up, as it goes.
pub(crate) struct SetupRun {
    pub(crate) host: String,
    /// each step, and what was said about it.
    pub(crate) steps: Vec<(String, Vec<String>)>,
    pub(crate) done: Option<Result<(), String>>,
    /// show it once it's ready (it was just added).
    pub(crate) switch_after: bool,
    pub(crate) started: Instant,
}

pub(crate) struct HostForm {
    /// a vercel sandbox, not a machine reached over ssh.
    vercel: bool,
    pub(crate) name: Entity<InputState>,
    pub(crate) ssh: Entity<InputState>,
    pub(crate) key: Entity<InputState>,
    port: Entity<InputState>,
    vcpus: u32,
    /// a vercel token, when the keychain has none yet.
    token: Entity<InputState>,
    team: Entity<InputState>,
    project: Entity<InputState>,
    /// the keychain has a vercel token already (asked once, when the form opens).
    has_token: bool,
}

impl HostForm {
    pub(crate) fn set_vercel(&mut self, on: bool) {
        self.vercel = on;
    }

    pub(crate) fn is_vercel(&self) -> bool {
        self.vercel
    }

    pub(crate) fn vcpus(&self) -> u32 {
        self.vcpus
    }

    pub(crate) fn set_vcpus(&mut self, n: u32) {
        self.vcpus = n;
    }

    pub(crate) fn has_token(&self) -> bool {
        self.has_token
    }

    pub(crate) fn token_input(&self) -> &Entity<InputState> {
        &self.token
    }

    pub(crate) fn team_input(&self) -> &Entity<InputState> {
        &self.team
    }

    pub(crate) fn project_input(&self) -> &Entity<InputState> {
        &self.project
    }
}

enum Progress {
    Said(Said),
    Done(Box<Host>, Result<(), String>),
}

/// what a machine's tile shows: this mac, a server, or a sandbox in the cloud.
fn machine_icon(host: Option<&Host>) -> &'static str {
    match host {
        None => "laptop",
        Some(h) if h.vercel.is_some() => "cloud",
        Some(_) => "server",
    }
}

impl HostsView {
    pub(crate) fn new(home: PathBuf) -> Self {
        Self { list: Hosts::load(&home), net: tessel_remote::net::Net::load(&home), home, ..Default::default() }
    }
}

impl Shell {
    pub(crate) fn hosts_reload(&mut self) {
        self.hosts.list = Hosts::load(&self.hosts.home);
        self.hosts.net = tessel_remote::net::Net::load(&self.hosts.home);
    }

    /// change how sandboxes are reached: saved, told to the sandboxes that
    /// are running (both ends have to agree), and the one shown connected again.
    fn set_net(&mut self, change: impl FnOnce(&mut tessel_remote::net::Net), window: &mut Window, cx: &mut Context<Self>) {
        let mut net = self.hosts.net.clone();
        change(&mut net);
        if let Err(e) = net.save(&self.hosts.home) {
            self.hosts.said = Some(format!("{e:#}"));
            cx.notify();
            return;
        }
        self.hosts.net = net;
        let sandboxes: Vec<Host> = self.hosts.list.hosts.iter().filter(|h| h.vercel.is_some()).cloned().collect();
        let shown = self.hosts.list.current.clone().filter(|id| sandboxes.iter().any(|h| &h.id == id));
        if sandboxes.is_empty() {
            self.hosts.said = Some("Saved. Sandboxes added from now on are reached this way.".into());
            cx.notify();
            return;
        }
        self.hosts.said = Some("Telling the sandboxes that are running…".into());
        let home = self.hosts.home.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let told = tokio::runtime::Builder::new_current_thread().enable_all().build().map(|rt| {
                rt.block_on(async {
                    let mut n = 0;
                    for h in &sandboxes {
                        if matches!(tessel_remote::vercel::reconfigure(&home, h).await, Ok(true)) {
                            n += 1;
                        }
                    }
                    n
                })
            });
            let _ = tx.send(told.unwrap_or(0));
        });
        cx.spawn_in(window, async move |this, cx| {
            let n = rx.await.unwrap_or(0);
            let _ = in_window(&this, cx, |this, _, cx| {
                this.hosts.said = Some(match n {
                    0 => "Saved. Sandboxes get it when they next start.".into(),
                    1 => "Saved, and the running sandbox started its service again with it.".into(),
                    n => format!("Saved, and {n} running sandboxes started their service again with it."),
                });
                // the one shown, connected again the new way.
                if let Some(id) = shown {
                    this.cmd(Cmd::Host(Some(id)));
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// what the window shows, by name.
    pub(crate) fn host_label(&self) -> String {
        match self.hosts.list.current.as_deref() {
            None => "This Mac".to_owned(),
            Some(id) => self.hosts.list.get(id).map_or_else(|| id.to_owned(), |h| h.name.clone()),
        }
    }

    pub(crate) fn on_host(&self) -> bool {
        self.hosts.list.current.is_some()
    }

    /// show another machine: `None` is this mac.
    pub(crate) fn switch_host(&mut self, to: Option<String>, cx: &mut Context<Self>) {
        self.hosts_reload();
        if self.hosts.list.current == to {
            return;
        }
        self.hosts.list.current = to.clone();
        if let Err(e) = self.hosts.list.save(&self.hosts.home) {
            self.error = Some(format!("Couldn't save which machine to show: {e}"));
        }
        self.let_go_of_host();
        self.cmd(Cmd::Host(to));
        cx.notify();
    }

    /// forget what the last machine had: its chats, terminals and pages aren't this one's.
    fn let_go_of_host(&mut self) {
        self.model.open = None;
        self.model.others.clear();
        self.model.set_sessions(Vec::new());
        self.model.connection = crate::model::Connection::Connecting;
        self.awaiting = None;
        self.views.clear();
        self.followed.clear();
        self.stashed.clear();
        self.scripts.clear();
        self.scripts_tried.clear();
        self.terms.clear();
        self.pane_terms.clear();
        self.ws_session = None;
        self.have_sessions = false;
        self.new_project = None;
        self.reflow_sidebar();
    }

    pub(crate) fn host_connected(&mut self, host: Option<String>, via: String) {
        self.hosts_reload();
        self.hosts.on = host;
        self.hosts.via = via;
    }

    /// set a host up (or bring it up to date), off the ui thread, showing each step.
    pub(crate) fn host_setup(&mut self, id: String, opts: Options, switch_after: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.hosts.run.as_ref().is_some_and(|r| r.done.is_none()) {
            return;
        }
        let Some(mut host) = Hosts::load(&self.hosts.home).get(&id).cloned() else { return };
        let home = self.hosts.home.clone();
        self.hosts.run = Some(SetupRun { host: id, steps: Vec::new(), done: None, switch_after, started: Instant::now() });
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Progress>();
        let spawned = std::thread::Builder::new().name("tessel-host-setup".into()).spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    let _ = tx.send(Progress::Done(Box::new(host), Err(e.to_string())));
                    return;
                }
            };
            let said = tx.clone();
            let mut say = move |s: Said| {
                let _ = said.send(Progress::Said(s));
            };
            let result = rt.block_on(tessel_remote::setup::setup(&home, &mut host, &opts, &mut say));
            let _ = tx.send(Progress::Done(Box::new(host), result.map_err(|e| format!("{e:#}"))));
        });
        if let Err(e) = spawned {
            self.hosts.run = None;
            self.error = Some(e.to_string());
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            while let Some(p) = rx.recv().await {
                let mut batch = vec![p];
                while let Ok(more) = rx.try_recv() {
                    batch.push(more);
                }
                let alive = in_window(&this, cx, |this, _, cx| {
                    for p in batch {
                        this.host_progress(p, cx);
                    }
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn host_progress(&mut self, p: Progress, cx: &mut Context<Self>) {
        let Some(run) = self.hosts.run.as_mut() else { return };
        match p {
            Progress::Said(Said::Step(s)) => run.steps.push((s, Vec::new())),
            Progress::Said(Said::Note(n)) => match run.steps.last_mut() {
                Some((_, notes)) => notes.push(n),
                None => run.steps.push((n, Vec::new())),
            },
            Progress::Done(host, result) => {
                // what was learned is kept, even when a later step failed.
                let mut list = Hosts::load(&self.hosts.home);
                list.put((*host).clone());
                if let Err(e) = list.save(&self.hosts.home) {
                    self.error = Some(format!("Couldn't save the host: {e}"));
                }
                let ok = result.is_ok();
                let (switch, id) = (run.switch_after, run.host.clone());
                run.done = Some(result);
                self.hosts_reload();
                if ok && switch {
                    self.switch_host(Some(id), cx);
                } else if ok && self.hosts.list.current.as_deref() == Some(id.as_str()) {
                    // shown now: connect again, to the daemon as it is now.
                    self.cmd(Cmd::Host(Some(id)));
                }
            }
        }
    }

    /// the add form's fields, made when the page shows it (inputs need the window).
    pub(crate) fn ensure_host_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hosts.relay_input.is_none() {
            let url = match &self.hosts.net.relay {
                tessel_remote::net::Relay::Own(u) => u.clone(),
                _ => String::new(),
            };
            self.hosts.relay_input = Some(cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder("https://relay.example.com");
                s.set_value(url, window, cx);
                s
            }));
        }
        if let Some(which) = self.hosts.flow_next.take() {
            match which {
                Some(id) => self.setup_flow_bring(id, window, cx),
                None => self.setup_flow_new(window, cx),
            }
        }
        if !self.hosts.adding || self.hosts.form.is_some() {
            return;
        }
        let field = |placeholder: &'static str, value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut s = InputState::new(window, cx).placeholder(placeholder);
                s.set_value(value, window, cx);
                s
            })
        };
        let saved: serde_json::Value = std::fs::read(tessel_remote::vercel::config(&self.hosts.home)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let has_token = tessel_remote::vercel::token().is_some();
        self.hosts.form = Some(HostForm {
            vercel: false,
            name: field("Dev box", "", window, cx),
            ssh: field("you@devbox.example.com, or a Host from ~/.ssh/config", "", window, cx),
            key: field("~/.ssh/id_ed25519", "", window, cx),
            port: field("7731", &tessel_remote::quic::DEFAULT_PORT.to_string(), window, cx),
            vcpus: 2,
            token: cx.new(|cx| InputState::new(window, cx).placeholder("vcp_…").masked(true)),
            team: field("team_…", saved["team"].as_str().unwrap_or(""), window, cx),
            project: field("prj_…", saved["project"].as_str().unwrap_or(""), window, cx),
            has_token,
        });
        if let Some(f) = &self.hosts.form {
            f.ssh.update(cx, |s, cx| s.focus(window, cx));
        }
    }

    /// the setup flow, for a new box (opened at the next frame, which has the window).
    pub(crate) fn add_host_open(&mut self, cx: &mut Context<Self>) {
        self.hosts.flow_next = Some(None);
        cx.notify();
    }

    /// the box the form describes, checked (and its vercel token and project
    /// kept, if it's a sandbox): with its udp port. what's wrong, if anything is.
    pub(crate) fn form_host(&mut self, cx: &mut Context<Self>) -> Result<(Host, u16), String> {
        let Some(f) = self.hosts.form.as_ref() else { return Err("…".into()) };
        let text = |e: &Entity<InputState>, cx: &App| e.read(cx).value().trim().to_owned();
        let name = text(&f.name, cx);
        if f.vercel {
            if name.is_empty() {
                return Err("Name the sandbox: it's how Vercel knows it, and how it's found again.".into());
            }
            let token = text(&f.token, cx);
            if !token.is_empty() {
                tessel_remote::vercel::save_token(&token).map_err(|e| format!("Couldn't keep the token in the keychain: {e}"))?;
            }
            if !f.has_token && token.is_empty() {
                return Err("Paste a Vercel token (vercel.com › Account Settings › Tokens).".into());
            }
            let (team, project) = (text(&f.team, cx), text(&f.project, cx));
            if team.is_empty() || project.is_empty() {
                return Err("Say which Vercel team and project the sandbox goes in.".into());
            }
            let cfg = serde_json::json!({"team": team, "project": project});
            std::fs::write(tessel_remote::vercel::config(&self.hosts.home), serde_json::to_vec_pretty(&cfg).unwrap_or_default()).map_err(|e| format!("Couldn't save the team and project: {e}"))?;
            Ok((Host::sandbox(&name, f.vcpus), tessel_remote::quic::DEFAULT_PORT))
        } else {
            let (ssh, port) = (text(&f.ssh, cx), text(&f.port, cx));
            if ssh.is_empty() {
                return Err("Say how ssh reaches it: you@host, or a Host from your ~/.ssh/config.".into());
            }
            let port = if port.is_empty() { tessel_remote::quic::DEFAULT_PORT } else { port.parse::<u16>().map_err(|_| format!("{port} isn't a port."))? };
            let mut host = Host::new(&name, &ssh);
            host.key = Some(text(&f.key, cx)).filter(|k| !k.is_empty());
            Ok((host, port))
        }
    }

    /// stop a sandbox now, off the ui thread (it would stop by itself soon after nothing shows it).
    fn host_stop(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(host) = self.hosts.list.get(id).cloned() else { return };
        if self.hosts.list.current.as_deref() == Some(id) {
            self.switch_host(None, cx);
        }
        let home = self.hosts.home.clone();
        let (tx, rx) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let r = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| e.to_string()).and_then(|rt| rt.block_on(tessel_remote::vercel::stop(&home, &host)).map_err(|e| format!("{e:#}")));
            let _ = tx.send(r);
        });
        self.hosts.said = Some("Stopping…".into());
        cx.spawn_in(window, async move |this, cx| {
            let r = rx.await.unwrap_or_else(|_| Err("it went away".into()));
            let _ = in_window(&this, cx, |this, _, cx| {
                this.hosts.said = Some(match r {
                    Ok(true) => "Stopped. Its disk is kept; showing it again starts it.".into(),
                    Ok(false) => "It wasn't running.".into(),
                    Err(e) => e,
                });
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn host_remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.hosts.confirm_remove.as_deref() != Some(id) {
            self.hosts.confirm_remove = Some(id.to_owned());
            cx.notify();
            return;
        }
        self.hosts.confirm_remove = None;
        if self.hosts.list.current.as_deref() == Some(id) {
            self.switch_host(None, cx);
        }
        let mut list = Hosts::load(&self.hosts.home);
        list.remove(id);
        if let Err(e) = list.save(&self.hosts.home) {
            self.error = Some(format!("Couldn't save the hosts: {e}"));
        }
        if self.hosts.run.as_ref().is_some_and(|r| r.host == id && r.done.is_some()) {
            self.hosts.run = None;
        }
        self.hosts_reload();
        cx.notify();
    }

    /// claude's sign-in, in a terminal on the host the window shows.
    fn host_sign_in(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_settings(cx);
        self.run_in_terminal("claude".into(), crate::ui::Module::Terminal, window, cx);
    }

    // ---- drawing ------------------------------------------------------------------

    /// the top of the sidebar: which machine this is (a card with its
    /// picture, its name and how it's doing), and the others to switch to.
    pub(crate) fn host_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let me = cx.entity().downgrade();
        let label = self.host_label();
        let remote = self.on_host();
        let down = matches!(self.model.connection, crate::model::Connection::Down(_));
        let connecting = !matches!(self.model.connection, crate::model::Connection::Connected);
        let working = self.model.sessions.iter().filter(|s| s.run_state.is_busy()).count();
        let chats = self.model.sessions.len();
        let mut about = vec![if chats == 1 { "1 chat".to_owned() } else { format!("{chats} chats") }];
        if working > 0 {
            about.push(format!("{working} working"));
        }
        if remote {
            about.push(self.hosts.via.clone());
        }
        let about = if down && remote {
            "Reconnecting…".to_owned()
        } else if connecting {
            "Connecting…".to_owned()
        } else {
            about.join(" · ")
        };
        let seed = self.hosts.list.current.clone().unwrap_or_else(|| "this-mac".into());
        let icon = machine_icon(self.hosts.list.current());
        let picker = Popover::new("host-picker-popover")
            .p_0()
            .anchor(Anchor::TopLeft)
            .trigger(
                Button::new("host-picker")
                    .ghost()
                    .w_full()
                    .h(px(PICKER_H))
                    .px(px(8.))
                    .rounded(px(theme::R_CARD))
                    .when(!crate::glass::liquid(), |b| b.bg(p.fill.opacity(0.6)))
                    .border_1()
                    .border_color(p.separator)
                    .tooltip("Which machine the window shows")
                    .child(
                        h_flex()
                            .w_full()
                            .min_w_0()
                            .gap(px(10.))
                            .child(super::avatar::machine(&seed, icon, 28., 7.))
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap(px(1.))
                                    .child(div().truncate().set(Type::Body).font_weight(FontWeight::SEMIBOLD).text_color(p.label).child(label))
                                    .child(div().truncate().set(Type::Subheadline).text_color(if down && remote { p.blocked } else { p.tertiary }).child(about)),
                            )
                            .child(Icon::empty().path("icons/chevrons-up-down.svg").size(z(14.)).text_color(p.tertiary)),
                    ),
            )
            .content(move |_, _, cx| {
                let popover = cx.entity();
                let Some(shell) = me.upgrade() else { return div().into_any_element() };
                let this = shell.read(cx);
                let p = this.palette.get();
                let current = this.hosts.list.current.clone();
                let go = |to: Option<String>| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| this.switch_host(to.clone(), cx));
                    }
                };
                let machine = |id: SharedString, seed: String, icon: &'static str, name: String, note: String, on: bool| {
                    h_flex()
                        .id(id)
                        .test_support()
                        .min_h(px(44.))
                        .px(px(8.))
                        .gap(px(10.))
                        .rounded(px(theme::R_TILE))
                        .when(on, |d| d.bg(p.selection))
                        .when(!on, |d| d.hover(|d| d.bg(p.hover)))
                        .child(super::avatar::machine(&seed, icon, 26., 6.))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().truncate().set(Type::Body).font_weight(FontWeight::MEDIUM).text_color(p.label).child(name))
                                .child(div().truncate().set(Type::Subheadline).text_color(p.tertiary).child(note)),
                        )
                };
                let mut list = v_flex().w(px(280.)).p(px(6.)).gap(px(2.)).child(
                    machine("host-mac".into(), "this-mac".into(), "laptop", "This Mac".into(), "chats and agents on this machine".into(), current.is_none()).on_click(go(None)),
                );
                for h in &this.hosts.list.hosts {
                    let note = if !h.ready() {
                        "not set up yet".to_owned()
                    } else {
                        let how = match h.transport {
                            Transport::Quic => "quic",
                            Transport::Ssh => "through ssh",
                            Transport::Vercel => "vercel sandbox",
                        };
                        format!("{} · {how}", h.arch.as_deref().map_or_else(|| "linux".to_owned(), |a| format!("linux {a}")))
                    };
                    let on = current.as_deref() == Some(h.id.as_str());
                    list = list.child(machine(SharedString::from(format!("host-{}", h.id)), h.id.clone(), machine_icon(Some(h)), h.name.clone(), note, on).on_click(go(Some(h.id.clone()))));
                }
                let settings = |adding: bool| {
                    let (me, popover) = (me.clone(), popover.clone());
                    move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                        popover.update(cx, |st, cx| st.dismiss(window, cx));
                        let _ = me.update(cx, |this, cx| {
                            if adding {
                                this.add_host_open(cx);
                            } else {
                                this.settings_section = "hosts";
                                this.open_settings(cx);
                            }
                        });
                    }
                };
                list.child(
                    h_flex()
                        .id("host-add")
                        .test_support()
                        .min_h(px(40.))
                        .px(px(8.))
                        .gap(px(10.))
                        .rounded(px(theme::R_TILE))
                        .hover(|d| d.bg(p.hover))
                        .child(div().size(px(26.)).flex_shrink_0().flex().items_center().justify_center().rounded(px(6.)).bg(p.fill).child(Icon::empty().path("icons/plus.svg").size(z(14.)).text_color(p.label)))
                        .child(div().set(Type::Body).text_color(p.label).child("Connect a dev box"))
                        .on_click(settings(true)),
                )
                .child(theme::float_sep(&p))
                .child(theme::float_item(&p, "host-manage", Some("settings-2"), "Manage hosts…", None, false).test_support().on_click(settings(false)))
                .into_any_element()
            });
        // a pane of glass of its own, in liquid glass.
        div().relative().child(super::glass_spot(theme::R_CARD)).child(picker).into_any_element()
    }

    /// Settings › Hosts.
    pub(crate) fn hosts_settings(&self, cx: &mut Context<Self>) -> Div {
        let p = self.palette.get();
        let current = self.hosts.list.current.clone();
        let card = || v_flex().flex_shrink_0().rounded(px(theme::R_CARD)).border_1().border_color(p.separator).bg(p.window).overflow_hidden();
        let connected = |id: &str| self.hosts.on.as_deref() == Some(id) && matches!(self.model.connection, crate::model::Connection::Connected);

        // this mac, then each host: what it is, how it's doing, and one menu.
        let mut machines = card().child(self.machine_row("mac", "this-mac", "laptop", "This Mac".into(), "Chats and agents on this Mac".into(), current.is_none(), None, cx));
        for h in &self.hosts.list.hosts {
            let showing = current.as_deref() == Some(h.id.as_str());
            let mut about = if !h.ready() {
                "Not set up yet".to_owned()
            } else {
                match (&h.vercel, h.transport) {
                    (Some(v), _) => format!("Vercel sandbox · {} CPUs", v.vcpus),
                    (None, Transport::Ssh) => format!("{} · through ssh", h.ssh),
                    (None, _) => format!("{} · QUIC", h.ssh),
                }
            };
            if showing && connected(&h.id) {
                about = format!("{about} · {}", self.hosts.via);
            }
            let icon = if h.vercel.is_some() { "cloud" } else { "server" };
            machines = machines.child(self.machine_row(&h.id, &h.id, icon, h.name.clone(), about, showing, Some(h), cx));
            if let Some(run) = self.hosts.run.as_ref().filter(|r| r.host == h.id) {
                machines = machines.child(self.setup_steps(run));
            }
        }
        machines = machines.child(
            h_flex()
                .id("host-add-open")
                .test_support()
                .h(px(56.))
                .px_4()
                .gap_3()
                .border_t_1()
                .border_color(p.separator)
                .hover(|d| d.bg(p.hover))
                .on_click(cx.listener(|this, _, window, cx| this.setup_flow_new(window, cx)))
                .child(div().size(px(30.)).flex_shrink_0().flex().items_center().justify_center().rounded(px(8.)).bg(p.fill).child(Icon::empty().path("icons/plus.svg").size(z(15.)).text_color(p.label)))
                .child(v_flex().child(div().set(Type::Body).child("Add a dev box")).child(div().set(Type::Subheadline).text_color(p.secondary).child("A machine over ssh, or a Vercel sandbox"))),
        );
        v_flex()
            .gap_3()
            .child(machines)
            .children(self.hosts.said.clone().map(|t| div().px_1().set(Type::Callout).text_color(p.secondary).child(t)))
            .child(div().pt_2().px_1().set(Type::Headline).child("Connections"))
            .child(card().child(div().mt(px(-1.)).child(self.net_settings(cx))))
    }

    /// one machine in Settings › Hosts: its tile, name and state; clicking shows it.
    #[allow(clippy::too_many_arguments)]
    fn machine_row(&self, id: &str, seed: &str, icon: &'static str, name: String, about: String, showing: bool, host: Option<&Host>, cx: &mut Context<Self>) -> AnyElement {
        let p = self.palette.get();
        let to = host.map(|h| h.id.clone());
        let ready = host.is_none_or(Host::ready);
        let group = SharedString::from(format!("host-row-{id}"));
        h_flex()
            .id(group.clone())
            .test_support()
            .group(group.clone())
            .h(px(56.))
            .px_4()
            .gap_3()
            .when(host.is_some(), |d| d.border_t_1().border_color(p.separator))
            .when(!showing && ready, |d| {
                let to = to.clone();
                d.hover(|d| d.bg(p.hover)).on_click(cx.listener(move |this, _, _, cx| this.switch_host(to.clone(), cx)))
            })
            .child(super::avatar::machine(seed, icon, 30., 8.))
            .child(v_flex().flex_1().min_w_0().child(div().set(Type::Body).truncate().child(name)).child(div().set(Type::Subheadline).text_color(p.secondary).truncate().child(about)))
            .when(showing, |d| d.child(h_flex().gap_1().child(Icon::empty().path("icons/check.svg").size(z(13.)).text_color(p.accent)).child(div().set(Type::Subheadline).text_color(p.secondary).child("Showing"))))
            .children(host.map(|h| self.host_menu(h, showing, cx)))
            .into_any_element()
    }

    /// what can be done to a host, in one menu.
    fn host_menu(&self, h: &Host, showing: bool, cx: &mut Context<Self>) -> AnyElement {
        let me = cx.entity().downgrade();
        let (id, ready, sandbox) = (h.id.clone(), h.ready(), h.vercel.is_some());
        div()
            .id(SharedString::from(format!("host-{id}-menu-zone")))
            .on_click(|_, _, cx| cx.stop_propagation())
            .child(
                Popover::new(SharedString::from(format!("host-{id}-menu")))
                    .p_0()
                    .anchor(Anchor::TopRight)
                    .trigger(Button::new(SharedString::from(format!("host-{id}-more"))).ghost().small().icon(Icon::empty().path("icons/ellipsis.svg")).tooltip("More"))
                    .content(move |_, _, cx| {
                        let popover = cx.entity();
                        let Some(shell) = me.upgrade() else { return div().into_any_element() };
                        let this = shell.read(cx);
                        let p = this.palette.get();
                        let armed = this.hosts.confirm_remove.as_deref() == Some(id.as_str());
                        let busy = this.hosts.run.as_ref().is_some_and(|r| r.done.is_none());
                        let act = |f: Box<dyn Fn(&mut Shell, &mut Window, &mut Context<Shell>)>, close: bool| {
                            let (me, popover) = (me.clone(), popover.clone());
                            move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                                if close {
                                    popover.update(cx, |st, cx| st.dismiss(window, cx));
                                }
                                let _ = me.update(cx, |this, cx| f(this, window, cx));
                            }
                        };
                        let i = |s: &str| SharedString::from(format!("host-{id}-{s}"));
                        let mut list = theme::float_list(&p).w(px(232.));
                        if ready && !showing {
                            let to = id.clone();
                            list = list.child(theme::float_item(&p, i("show"), Some("monitor"), "Show", None, false).test_support().on_click(act(Box::new(move |this, _, cx| this.switch_host(Some(to.clone()), cx)), true)));
                        }
                        if ready {
                            let to = id.clone();
                            list = list.child(theme::float_item(&p, i("bring"), Some("package"), "Bring your Claude…", None, false).test_support().on_click(act(Box::new(move |this, w, cx| this.setup_flow_bring(to.clone(), w, cx)), true)));
                        }
                        if showing {
                            list = list.child(theme::float_item(&p, i("sign-in"), Some("log-in"), "Sign in to Claude there", None, false).test_support().on_click(act(Box::new(|this, w, cx| this.host_sign_in(w, cx)), true)));
                        }
                        if !busy {
                            let to = id.clone();
                            list = list.child(
                                theme::float_item(&p, i("setup"), Some("refresh-cw"), if ready { "Update tessel there" } else { "Set up" }, None, false)
                                    .test_support()
                                    .on_click(act(Box::new(move |this, w, cx| this.host_setup(to.clone(), Options::default(), false, w, cx)), true)),
                            );
                        }
                        if sandbox && ready {
                            let to = id.clone();
                            list = list.child(theme::float_item(&p, i("stop"), Some("square"), "Stop the sandbox", None, false).test_support().on_click(act(Box::new(move |this, w, cx| this.host_stop(&to, w, cx)), true)));
                        }
                        let to = id.clone();
                        list.child(theme::float_sep(&p))
                            .child(
                                theme::float_danger(&p, i("remove"), Some("trash"), if armed { "Click again to remove" } else { "Remove" }, armed.then_some("Tessel stays installed there"))
                                    .test_support()
                                    .on_click(act(Box::new(move |this, _, cx| this.host_remove(&to, cx)), armed)),
                            )
                            .into_any_element()
                    }),
            )
            .into_any_element()
    }

    /// Settings › Hosts › Sandbox connections: how sandboxes are reached.
    fn net_settings(&self, cx: &mut Context<Self>) -> Div {
        use tessel_remote::net::{Relay, Via};
        let p = self.palette.get();
        let net = &self.hosts.net;
        let via = match net.via {
            Via::Auto => "auto",
            Via::Quic => "quic",
            Via::Ws => "ws",
        };
        let via_about = match net.via {
            Via::Auto => "QUIC straight to the sandbox, WebSockets if that can't get through.",
            Via::Quic => "QUIC only: it fails rather than going slow.",
            Via::Ws => "Through Vercel's HTTPS address. No UDP or relay, about a third slower.",
        };
        let relay = match &net.relay {
            Relay::N0 => "n0",
            Relay::Own(_) => "own",
            Relay::None => "none",
        };
        let relay_about = match &net.relay {
            Relay::N0 => "iroh's public relays carry the first moment of a connection. They never see what's said.",
            Relay::Own(_) => "Your own relay: nothing goes through anyone else's servers.",
            Relay::None => "Direct only: slower to connect, and fails behind strict networks.",
        };
        let quic = net.via != Via::Ws;
        let mut block = v_flex()
            .child(self.setting_row(
                "Connect by",
                via_about,
                self.segmented("net-via", &[("auto", "Auto"), ("quic", "QUIC"), ("ws", "WebSockets")], via, cx, |this, v, w, cx| {
                    this.set_net(|n| n.via = match v { "quic" => Via::Quic, "ws" => Via::Ws, _ => Via::Auto }, w, cx)
                }),
            ));
        if quic {
            block = block.child(self.setting_row(
                "Relay",
                relay_about,
                self.segmented("net-relay", &[("n0", "iroh's"), ("own", "Your Own"), ("none", "None")], relay, cx, |this, v, w, cx| {
                    let typed = this.hosts.relay_input.as_ref().map(|i| i.read(cx).value().trim().to_owned()).unwrap_or_default();
                    match v {
                        "own" if typed.is_empty() => {
                            this.hosts.said = Some("Write your relay's address below, then Use It.".into());
                            this.hosts.relay_own_next = true;
                            cx.notify();
                        }
                        "own" => this.set_net(|n| n.relay = Relay::Own(typed), w, cx),
                        "none" => this.set_net(|n| n.relay = Relay::None, w, cx),
                        _ => this.set_net(
                            |n| {
                                n.relay = Relay::N0;
                                n.lookup = true;
                            },
                            w,
                            cx,
                        ),
                    }
                }),
            ));
            if (matches!(net.relay, Relay::Own(_)) || self.hosts.relay_own_next)
                && let Some(input) = &self.hosts.relay_input
            {
                block = block.child(
                    h_flex()
                        .px_4()
                        .pb_2()
                        .gap_2()
                        .child(div().flex_1().rounded(px(theme::R_TILE)).border_1().border_color(p.separator).bg(p.raised).px_2().py_1().child(div().id("net-relay-url").test_support().child(Input::new(input).appearance(false))))
                        .child(theme::button(&p, "net-relay-use", "Use It", None, ButtonKind::Secondary).test_support().on_click(cx.listener(|this, _, window, cx| {
                            let url = this.hosts.relay_input.as_ref().map(|i| i.read(cx).value().trim().to_owned()).unwrap_or_default();
                            this.hosts.relay_own_next = false;
                            this.set_net(|n| n.relay = Relay::Own(url), window, cx);
                        }))),
                );
            }
            let own = matches!(net.relay, Relay::Own(_));
            let lookup = net.lookup;
            block = block.child(self.setting_row(
                "Public lookup",
                if own { "Finds a sandbox by its key at dns.iroh.link. Off, it's found at your relay." } else { "Finds a sandbox by its key at dns.iroh.link. Turning it off needs your own relay." },
                Switch::new("net-lookup").checked(lookup).disabled(!own).on_click(cx.listener(|this, on: &bool, window, cx| {
                    let on = *on;
                    this.set_net(|n| n.lookup = on, window, cx);
                })),
            ));
        }
        block
    }

    fn setup_steps(&self, run: &SetupRun) -> Div {
        let p = self.palette.get();
        let mut list = v_flex().px_4().pb_3().pl(px(44.)).gap(px(4.));
        let n = run.steps.len();
        for (i, (step, notes)) in run.steps.iter().enumerate() {
            let last = i + 1 == n;
            let (icon, ink) = match (&run.done, last) {
                (None, true) => ("loader-circle", p.accent),
                (Some(Err(_)), true) => ("circle-x", p.failed),
                _ => ("circle-check", p.finished),
            };
            list = list.child(
                h_flex()
                    .gap_2()
                    .child(Icon::empty().path(format!("icons/{icon}.svg")).size(z(13.)).text_color(ink))
                    .child(div().set(Type::Callout).text_color(if last && run.done.is_none() { p.label } else { p.secondary }).child(step.clone())),
            );
            for note in notes {
                list = list.child(div().pl(px(21.)).set(Type::Subheadline).text_color(p.tertiary).child(note.clone()));
            }
        }
        match &run.done {
            Some(Ok(())) => list.child(div().pt_1().set(Type::Callout).text_color(p.finished).child(format!("Ready, in {}s.", run.started.elapsed().as_secs()))),
            Some(Err(e)) => list.child(div().pt_1().set(Type::Callout).text_color(p.failed).child(e.clone())),
            None if run.steps.is_empty() => list.child(div().set(Type::Callout).text_color(p.secondary).child("Starting…")),
            None => list,
        }
    }


}
